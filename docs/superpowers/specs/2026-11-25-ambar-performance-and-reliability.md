# Спецификация: Устранение проблем производительности и надёжности агента (ревизия 2)

**Дата:** 2026-11-25  
**Статус:** Утверждено после архитектурной ревизии (готово к реализации)  
**Область:** агент (`src/agent/**`), CSI-сервисы (`src/csi/**`), storage (`src/backing.rs`, `src/placement.rs`), управление процессами и образом (`src/exec.rs`, `image/Dockerfile`, `deploy/daemonset.yaml`), рантайм (`src/main.rs`, `src/metrics.rs`).  
**Целевое железо:** Кластер 3× FriendlyElec NanoPi R5C, Debian 13 trixie, ядро 6.12+ (`aarch64`), модуль DRBD 9.3.4, ≤ 32 томов.

---

## 1. Контекст и результаты ревизии исходного аудита

Первичный кодовый аудит выявил 30 кандидатов на улучшение (`К1–К5`, `В1–В11`, `Н1–Н14`). Сверка каждого пункта с фактической реализацией, тестами и архитектурными инвариантами проекта (`AGENTS.md`, сессии hardening `e7a7c4d` / `ceb7ba8` / `eba32aa`) показала, что:

1. **Реальное узкое место по CPU/IO и сети** сосредоточено в секундном failure-цикле (`tick_failures`), лишних живых `LIST` на холодном старте и повторных проверках `status`/`pods` при очистке `Client`-томов.
2. **Реальные дефекты надёжности** — накопление осиротевших процессов `drbdmeta`/`drbdsetup` при таймауте (когда `ambar` работает как PID 1), кросс-процессная гонка `mkfs.ext4` при rolling restart DaemonSet, head-of-line blocking очереди `serve` на время `mkfs`/`mount` и неограниченное чтение pipe в `exec::drain`.
3. **Ряд пунктов первичного аудита оказался ошибочным или опасным** (см. раздел 3) — их реализация нарушила бы семантику DRBD 9, инварианты `exec.rs`, fail-closed модель отказов или CSI-спецификацию. Ещё несколько пунктов дают выигрыш порядка десятков наносекунд при `n ≤ 32` ценой усложнения кода (YAGNI).

---

## 2. Подтверждённый каталог задач (к реализации)

Приоритеты: **К** — критично, **В** — важно, **Н** — точечное улучшение.

| ID | Приоритет | Проблема | Локация |
|---|---|---|---|
| К1 | критично | `run_failure_loop` крутится на 1 Гц и каждый тик делает тяжёлую работу без дебаунса подсистем | `src/agent/run.rs:1606-1756` |
| В1 | важно | `mounted_pod_uids_for_volume` делает синхронный `read_dir` + `exists` на каждый том прямо в async | `src/agent/kube.rs:390-406`, `src/agent/run.rs:1636` |
| В2 | важно | `check_and_cleanup_client` каждую секунду берёт лок, повторно вызывает `read_status` (`drbdsetup`) и делает живой `Pod` LIST | `src/agent/cleanup.rs:52-96`, `src/agent/run.rs:1704-1710` |
| В4 | важно | Синхронный `read_to_string("/sys/module/drbd/version")` вызывается каждый тик прямо в async | `src/agent/run.rs:1713-1715` |
| В8 | важно | `QuorumTracker` и `StandaloneTracker` не удаляют записи исчезнувших томов (`forget` не вызывается) | `src/agent/quorum.rs:66-68`, `src/agent/standalone.rs:103-105`, `src/agent/run.rs:1606-1738` |
| К5 | критично | `reconcile_one` вызывает `volumes.list()` на каждый том → до прогрева кэша даёт `N + 1` живых сетевых LIST на старте | `src/agent/run.rs:803-809, 1520-1530, 1564-1585` |
| В3 | важно | `exec::drain` читает `read_to_end` в неограниченный `Vec<u8>` → риск OOM при runaway-выводе | `src/exec.rs:324-338` |
| Н8 | низко | Best-effort `blkdiscard` выполняется с дефолтным таймаутом 120 с вместо короткого лимита | `src/backing.rs:375, 391` |
| В5 | важно | `ControllerService::find_or_create` держит глобальный мьютекс `placing` во время сетевого `nodes.load()` и первичного `volumes.get()` | `src/csi/controller.rs:204-243` |
| В6 | важно | Неограниченный параллелизм тяжёлых CSI Node-вызовов в `detached` | `src/csi/node.rs:485-499, 1054-1135` |
| Н6 | низко | `std::fs::create_dir_all(&staging)` вызывается синхронно в async `stage_locked` | `src/csi/node.rs:778` |
| К3 | критично | Head-of-line blocking: серийный `serve`-цикл ждёт освобождения `VolumeLocks`, пока идёт долгий `mkfs`/`mount` одного тома | `src/agent/run.rs:247-261, 790-798, 1577-1585` |
| В7 | важно | Кросс-процессная гонка `mkfs.ext4` при rolling restart DaemonSet (отсутствует файловый `flock` и повторная сверка сигнатуры под ним) | `src/csi/node.rs:764-777`, `src/reconcile.rs:706-756` |
| К2 | критично | При таймауте команды осиротевшие внуки (`drbdmeta`/`drbdsetup`) перепривязываются к PID 1 (`ambar agent`), который их не реапит | `image/Dockerfile:46-61`, `deploy/daemonset.yaml:50` |
| Н4 | низко | Ретраи JSON Patch при `409 Conflict` / `422` выполняются без backoff (мгновенно сжигают попытки при гонке 3 узлов) | `src/agent/kube.rs:227-258, 706-758`, `src/agent/cleanup.rs:100-125` |
| Н7 | низко | `MetricsHolder` клонирует всю строку метрик под read-lock на каждый скрейп и молча игнорирует poisoned lock | `src/metrics.rs:38-58`, `src/agent/metrics_server.rs:32-77` |
| Н12 | низко | Самопроверка страницы `jemalloc` выполняется после старта модуля и `tokio::runtime` | `src/main.rs:180-221` |
| Н13 + В11 | низко | Дублирование `free_budgets` / `free_budgets_aligned` + недокументированное требование стандартного размера экстента VG (`PE = 4 MiB`) | `src/placement.rs:144-194`, `README.md`, `README.ru.md` |

---

## 3. Отклонённые и пересмотренные пункты исходного аудита

Следующие пункты **исключены из плана реализации**, так как несут риск регрессий или неоправданного усложнения:

1. **Н9 (`membership.rs:60` — «исправить `>` на `>=` в проверке `max_peers`») — ОТКЛОНЕНО (ложный баг).**  
   В DRBD 9 параметр `max-peers` задаёт число *соседей* (пиров), поэтому максимальное число узлов в ресурсе равно `max_peers + 1`. Это поведение проверено тестом `join_fills_the_list_up_to_max_peers_plus_one` (`src/tests/membership.rs:154`). Замена `>` на `>=` сломает модель DRBD и существующие тесты.
2. **К2 вариант (a) (`PR_SET_CHILD_SUBREAPER` + фоновая петля `waitpid(-1, WNOHANG)`) — ОТКЛОНЕНО (нарушает инвариант `exec.rs`).**  
   Модуль `src/exec.rs:289-315` использует `waitid(..., WNOWAIT)`, чтобы лидер группы оставался нереапнутым до отправки `kill_group(-pgid, SIGKILL)`. Фоновый `waitpid(-1, WNOHANG)` в том же процессе перехватит прямых детей `Command::spawn` раньше `supervise`, вызвав `ECHILD` и риск переиспользования PGID. Задача К2 решается строго вариантом **(b)** — запуском через `tini` как PID 1.
3. **В10 («при ошибке `read_all_statuses` продолжать eviction/taint по устаревшему снимку») — ОТКЛОНЕНО (нарушает fail-closed).**  
   Согласно решению аудита безопасности `ceb7ba8`, сбой чтения статуса DRBD обязан прерывать тик в режиме fail-closed (сохраняя текущие taints и метрики без новых разрушающих действий). Выселение подов по устаревшему снимку при временном сбое `drbdsetup` способно убить рабочие поды на уже восстановившемся кворуме.
4. **Н1 (`mount.rs:306-322` — «добавить backoff и `umount -l` как последнее средство») — ОТКЛОНЕНО (опасно для данных).**  
   Цикл `0..MAX_UMOUNT_ATTEMPTS` в `unmount` разбирает стек вложенных маунтов, а не ретраит ошибки. Применение `umount -l` (lazy unmount) скрывает маунт из VFS, оставляя блочное устройство DRBD открытым (`open: true`), что ломает контракт `NodeUnstageVolume`.
5. **Н10 («удалять `staging_target_path` после unstage») и Н11 («отменять `detached` по `CancellationToken`») — ОТКЛОНЕНО.**  
   По спецификации CSI жизненным циклом директории `staging_target_path` управляет `kubelet` (драйвер удаляет только `target_path` в `NodeUnpublishVolume`). А задача `detached` специально защищает начатые `mkfs`/`mount`/`unstage` от обрыва на полпути в пределах `terminationGracePeriodSeconds: 150`.
6. **К4 (перевод `Drbd::status(vol)` на `drbdsetup status --json <vol>`), В9 (`VolumeLocks::retain`), Н2 (`create-md` timeout), Н3 (`BinaryHeap` в `Queue`), Н5 (`POLL_INTERVAL`), Н14 (сортировка в `status_matches_spec`) — ОТЛОЖЕНО (YAGNI при `n ≤ 32`).**  
   На целевом кластере (`≤ 32` томов) `drbdsetup status --json` читает состояние из netlink в памяти (~4 КиБ JSON, `< 2 мс`), а смена CLI-вызова требует платформенной обработки кода `10` (`No such resource`) и переписывания десятков `FakeRunner`-фикстур. Лишние вызовы `drbdsetup` в фоновом цикле полностью устраняются переиспользованием снимка `statuses` в рамках К1/В2. Аналогично, `HashMap::retain` и `BTreeMap::min_by_key` на 32 элементах занимают `< 100 нс`.

---

## 4. Архитектурные решения по группам

### 4.1 Разгрузка `tick_failures`, дебаунс cleanup и очистка трекеров (К1, В1, В2, В4, В8)

1. **Интервал цикла отказов:** увеличить шаг `run_failure_loop` с `1s` до `2s` (`FAILURE_LOOP_INTERVAL = Duration::from_secs(2)`). При `quorum_loss_grace_period` 15–30 с погрешность обнаружения ≤ 2 с несущественна, а фоновая частота `fork/exec` падает вдвое.
2. **Обратный индекс смонтированных подов (В1, В4):** добавить `mounted_pod_uids_by_volume(kubelet_pods_dir: &Path) -> HashMap<String, Vec<String>>` в `src/agent/kube.rs` — один обход `/var/lib/kubelet/pods/<uid>/volumes/kubernetes.io~csi/<vol>` за тик внутри `tokio::task::spawn_blocking`. Существующую функцию `mounted_pod_uids_for_volume` оставить тонкой обёрткой или для точечных вызовов.
3. **Кэширование версии модуля DRBD (В4):** читать `/sys/module/drbd/version` один раз через `OnceLock<Option<String>>` (или в `spawn_blocking` до старта цикла), убрав синхронное чтение `/sys` из `tick_failures`.
4. **Дебаунс и предфильтрация cleanup stale-клиентов (В2):**
   - Ввести `CleanupTracker` (интервал `DEFAULT_CLEANUP_INTERVAL = Duration::from_secs(30)`, по образцу `TaintTracker`).
   - Передавать в `check_and_cleanup_client` предвычисленный снимок статуса ресурса (`Option<&ResourceStatus>`) и срез `mounted_uids: &[String]`:
     - если ресурс в снимке `Primary` или `open`, либо `!mounted_uids.is_empty()` — сразу возвращать `Ok(None)` **без** захвата `VolumeLocks`, без подпроцесса `drbdsetup` и без сетевого `has_node_pods_for_volume`;
     - только если по предварительному снимку клиент выглядит свободным, захватывать `agent.locks.lock(volume)`, повторно перечитывать свежий `agent.read_status(volume)` (защита от TOCTOU перед `drbdadm down`) и опрашивать `has_node_pods_for_volume`.
5. **Сборка мусора в трекерах (В8):** добавить метод `retain_volumes(&mut self, active: &HashSet<&str>)` в `QuorumTracker` и `StandaloneTracker` и вызывать его в `tick_failures` после успешного чтения `statuses`. При ошибке `read_all_statuses()` тик завершается досрочно (fail-closed), трекеры сохраняют состояние.

### 4.2 Снимок для проверки коллизий на полном проходе (К5)

В `Agent::all_names` агент уже вызывает `self.volumes.list().await?`. До прогрева reflector-кэша (`listed == false`) каждый последующий вызов `self.volumes.list()` внутри `reconcile_one` генерирует отдельный сетевой `LIST` (`N + 1` запросов на старте).

**Решение:**
- Изменить `all_names` → `all_names_and_peers(&self) -> Result<(BTreeSet<String>, Vec<(String, u32, u16)>), AgentError>`, возвращая вместе с именами снимок `(name, minor, port)` из того же единственного `list()`.
- В `serve` хранить опциональный снимок текущего полного прохода (сбрасывая его при поступлении событий из `watch` или по завершении стартового пакета) либо передавать `Option<&[(String, u32, u16)]>` во внутренний метод реконсиляции: если снимок передан — использовать его, иначе вызывать `self.volumes.list().await?` (после прогрева это дешёвое чтение из памяти `cache.state()`).

### 4.3 Лимит захвата вывода без `SIGPIPE` (В3) и таймаут `blkdiscard` (Н8)

1. **Лимит памяти в `exec::drain` (`src/exec.rs`):**
   - Ввести `pub const MAX_OUTPUT_CAPTURE: usize = 1024 * 1024;` (1 МиБ).
   - Читать в буфер не более `MAX_OUTPUT_CAPTURE` байт (`pipe.by_ref().take(MAX_OUTPUT_CAPTURE as u64 + 1).read_to_end(&mut bytes)`).
   - Если прочитано `> MAX_OUTPUT_CAPTURE` байт: обрезать `bytes` до `MAX_OUTPUT_CAPTURE`, дописать маркер `b"\n... [output truncated]"` и **обязательно дочитать остаток `pipe` в `io::sink()`** (`let _ = io::copy(&mut pipe, &mut io::sink());`), чтобы дочерний процесс не получил `SIGPIPE` при записи в закрытый пайп.
2. **Таймаут `blkdiscard` (`src/backing.rs`):**
   - Ввести `pub const BLKDISCARD_TIMEOUT: Duration = Duration::from_secs(15);` и вызывать `blkdiscard` через `checked_with_timeout(runner, "blkdiscard", &["--", &dev], BLKDISCARD_TIMEOUT)`, чтобы медленный discard не удерживал создание/удаление тома по 120 с.

### 4.4 Конкурентность CSI и асинхронный I/O (В5, В6, Н6)

1. **`ControllerService::find_or_create` (`src/csi/controller.rs`):**
   - До захвата `self.placing.lock().await` выполнять быструю идемпотентную проверку `self.volumes.get(name).await` и чтение карты узлов `self.nodes.load().await`.
   - Внутри критической секции `self.placing` оставить повторный `self.volumes.get(name)` (на случай параллельного создания), `self.volumes.list_fresh()`, `place(...)` и `self.volumes.create(...)` — это гарантирует отсутствие коллизий `minor`/`port` без удержания глобального лока на время чтения ConfigMap.
2. **Семафор на `NodeService` (`src/csi/node.rs`):**
   - Добавить в `NodeService` поле `concurrency: Arc<Semaphore>` с лимитом `MAX_CONCURRENT_NODE_OPS = 16`.
   - Захватывать `OwnedSemaphorePermit` перед запуском `detached(...)` в `node_stage_volume`, `node_unstage_volume`, `node_publish_volume`, `node_unpublish_volume` и передавать пермит внутрь фоновой задачи, чтобы лимит действовал до фактического завершения отсоединившейся операции.
3. **`create_dir_all` в `stage_locked` (`src/csi/node.rs:778`):**
   - Обернуть создание `staging`-директории в `tokio::task::spawn_blocking` аналогично `publish_locked`.

### 4.5 Кросс-процессная защита `mkfs` (В7) и устранение HoL-blocking в `serve` (К3)

1. **Файловый `flock` и повторная проверка сигнатуры вокруг `mkfs.ext4` (`src/csi/node.rs`):**
   - В ветке `Format::FormatThenMark` после `self.still_unformatted(&object, &volume).await?` выполнять форматирование внутри `self.agent.blocking(...)`:
     1. Взять кросс-процессный файловый лок тома `Reconciler::new(paths, runner).lock_volume(&volume)`: при `ReconcileError::Busy` вернуть `Status::unavailable("volume is locked by another process; retry")`.
     2. Под удерживаемым `flock` **повторно вызвать `Mount::new(runner, &mountinfo).signature(&target)`**:
        - `Ok(Signature::Unformatted)` → запустить `mkfs_ext4(&target)` под этим же `flock`;
        - `Ok(Signature::Ext4)` → другой под при rolling restart уже успел отформатировать том; пропустить `mkfs_ext4` и перейти к `mark_formatted`;
        - `Ok(Signature::Other(fs))` → вернуть `Status::failed_precondition(...)`;
        - `Err(e)` → вернуть `mount_status(e)`.
2. **Неблокирующий обход занятых томов в `serve` (`src/agent/run.rs`):**
   - Добавить `VolumeLocks::try_lock(&self, volume: &str) -> Option<OwnedMutexGuard<()>>`.
   - Разделить `reconcile_one` (публичный, ждёт `locks.lock(volume).await`) и внутренний `try_reconcile_one` для `serve`: если `locks.try_lock(volume)` вернул `None` (том занят долгой операцией CSI `stage`/`unstage`), `serve` **не ждёт**, планирует повтор `queue.schedule(&name, Instant::now() + BUSY_RETRY_DELAY)` (`1s`), **не вызывает** `startup.record(...)` и немедленно переходит к следующему тому в очереди.

### 4.6 Инициализатор PID 1 `tini` для reaping сирот (К2)

- В `image/Dockerfile` добавить пакет `tini` в `apt-get install` финального слоя `debian:trixie-slim`.
- В `deploy/daemonset.yaml` заменить `command: ["ambar", "agent"]` на `command: ["/usr/bin/tini", "--", "ambar", "agent"]`.
- `tini` как PID 1 автоматически собирает всех осиротевших внуков (`drbdmeta`, `drbdsetup`), перепривязанных к PID 1 после гибели лидера группы, пробрасывает `SIGTERM` процессу `ambar agent` и никак не вмешивается в `waitid(WNOWAIT)` прямых детей `ambar`.

### 4.7 Точечные улучшения качества и гигиены (Н4, Н7, Н12, Н13, В11)

- **Н4:** добавить экспоненциальный backoff (`20ms`, `40ms`, `80ms`, `160ms`) между попытками при `409 Conflict` / `422` в `edit_retrying`, `ensure_node_taint`, `remove_node_taint` (`src/agent/kube.rs`) и `leave_client` (`src/agent/cleanup.rs`).
- **Н7:** перевести `MetricsHolder` на `Arc<RwLock<Arc<str>>>` (`get(&self) -> Arc<str>`), обновив `metrics_server::serve_listener` на `Arc<dyn Fn() -> Arc<str> + Send + Sync>`, и заменить `if let Ok(...)` / `.ok()` на `unwrap_or_else(|e| e.into_inner())` для устойчивости к poisoned lock.
- **Н12:** перенести вызов `log_allocator_page_size()` в начало `run_agent` (`src/main.rs`) до `preflight`, загрузки модуля и сборки `tokio::runtime`.
- **Н13 + В11:** дедуплицировать `free_budgets` и `free_budgets_aligned` в `src/placement.rs` через общую функцию `budgets_by`, задокументировать требование стандартного размера физического экстента LVM (`PE = 4 MiB`, дефолт `vgcreate`) в `src/placement.rs`, `README.md` и `README.ru.md`.

---

## 5. Неприкосновенные инварианты безопасности

1. **Data-safety таблица (`src/agent/run.rs:7-18`):** удаление реплики/клиента — строго через `remove_checked` / `remove_unconfigured` под файловым `flock` с повторной проверкой статуса DRBD (`open == false`, `role != Primary`).
2. **Классификация DRBD-метаданных (`src/drbd/adm.rs:93-293`)** и запрет `create-md` при сконфигурированном ресурсе (`src/reconcile.rs:228-238`).
3. **Инвариант `src/exec.rs`:** лидер группы проверяется через `waitid(WNOWAIT)` и не реапится до вызова `kill_group(-pgid, SIGKILL)`.
4. **Fail-closed в `tick_failures` (`ceb7ba8`):** сбой `read_all_statuses()` немедленно прерывает тик, не снимая taints и не запуская эвикцию по устаревшим данным.
5. **Uid-precondition в JSON Patch (`src/agent/kube.rs`):** атомарность всех мутаций объектов K8s.

---

## 6. Порядок реализации и верификации

Все задачи независимы по коммитам и выполняются строго по TDD (падающий тест → минимальная реализация → `cargo fmt --check` + `cargo clippy --all-targets --no-deps -- -D warnings` + `cargo test` → очистка промежуточных тестовых артефактов при необходимости):

1. **Задача 1:** Гигиена рантайма, метрик и аллокации бюджета (`Н7`, `Н12`, `Н13`, `В11` + синхронное обновление `README.md`/`README.ru.md`).
2. **Задача 2:** Защита `exec::drain` от OOM без `SIGPIPE` и короткий таймаут `blkdiscard` (`В3`, `Н8`).
3. **Задача 3:** Backoff при конфликтах JSON Patch в K8s API (`Н4`).
4. **Задача 4:** Разгрузка `tick_failures`: обратный индекс маунтов, кэш версии DRBD, дебаунс cleanup и сборка мусора трекеров (`К1`, `В1`, `В2`, `В4`, `В8`).
5. **Задача 5:** Переиспользование снимка объектов для детекта коллизий на полном проходе (`К5`).
6. **Задача 6:** Сужение `placing`-лока в контроллере, семафор `detached` и `spawn_blocking` в `NodeService` (`В5`, `В6`, `Н6`).
7. **Задача 7:** Неблокирующий `try_lock` в `serve`-цикле против head-of-line blocking (`К3`).
8. **Задача 8:** Файловый `flock` и повторная проверка сигнатуры вокруг `mkfs.ext4` (`В7`).
9. **Задача 9:** Внедрение `tini` как PID 1 в образ и DaemonSet против зомби-сирот (`К2` + документация).
