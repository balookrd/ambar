# ambar, фаза 3: обработка отказов узлов и сети — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** драйвер корректно реагирует на отказы узлов и сети: при потере кворума на узле со смонтированным томом агент немедленно помечает узел taint `ambar.balookrd.github.io/no-quorum:NoSchedule` и по истечении grace period (по умолчанию 30 с) освобождает том путём удаления подов; при возникновении `StandAlone` агент в роли `Secondary` автоматически выполняет до 3 попыток `drbdadm adjust`; при неразрешённом `StandAlone` (split-brain) выставляется метрика ручного вмешательства; забытые записи `Client` без подов автоматически очищаются; файлы удаляемых томов без маркеров готовности безопасно проверяются через `losetup --read-only`; агент отдаёт метрики Prometheus на порту 9770.

**Архитектура:** все решения о taint, выселении подов, `adjust` и очистке клиентов остаются чистыми функциями (`agent::decide`, `agent::failure`), изолированными от прямого ввода-вывода. Взаимодействие с Kubernetes API осуществляется через трейт `Volumes` и расширенный клиент `KubeVolumes` (методы для taints, pods и read-only losetup). Экспорт метрик Prometheus работает на асинхронном легковесном HTTP-эндпоинте в составе процесса агента.

**Стек:** Rust (edition 2024), `tokio`, `kube` 5.1.0, `k8s-openapi` 0.29.0, `clap`; DRBD 9.3.4, drbd-utils 9.35.0; стенд Lima с k3s v1.37.2.

**Спека:** `docs/superpowers/specs/2026-11-12-ambar-design.md`. **Протокол опытов:** `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`. **Стенд:** `stand/README.md`. **План фазы 2б (исполнен):** `docs/superpowers/plans/2026-11-14-ambar-phase2b-replica-set.md`. **Журнал исполнения:** `.superpowers/sdd/progress.md` (строки этой фазы начинаются с `P3`).

## Место этой фазы

| Фаза | Что даёт | Состояние |
|---|---|---|
| 0. Образ и стенд | модуль DRBD 9 в образе, стенд из ВМ Lima | исполнена |
| 1. Ядро агента | согласование одного тома на узле через CLI | исполнена |
| 2а. Kubernetes | CRD, цикл согласования, CSI, манифесты; том через PVC | исполнена |
| 2б. Смена набора копий | добавление, удаление и перенос копии у существующего тома | исполнена |
| 3. Отказы узлов и сети (этот план) | освобождение тома при потере кворума, taint `no-quorum`, `adjust` при `StandAlone`, метрики, read-only проверка файлов, очистка `Client` | план написан |
| 4. Замер на R5C | память, `fsync`, сравнение с Longhorn | нужны узлы |

---

## Решения владельца и архитектурные инварианты (Фаза 3)

| № | Решение | Причина |
|---|---|---|
| R32 | Taint `ambar.balookrd.github.io/no-quorum` со значением `"true"` и эффектом `NoSchedule` ставится агентом на свой узел **немедленно**, как только хотя бы один смонтированный локально том теряет кворум (`devices[].quorum == false`). Taint снимается, когда кворум есть у всех смонтированных томов узла | Предотвратить назначение новых рабочих нагрузок на изолированный узел сразу же, не ожидая истечения таймера grace period |
| R33 | Grace period потери кворума: параметр агента `--quorum-loss-grace-period` (по умолчанию 30 с). По истечении таймера агент находит через Kubernetes API поды на своём узле (`spec.nodeName == local_node`), использующие данный том, и удаляет их (`Api<Pod>::delete`). Если API server недоступен, агент ждёт его возврата и ничего не делает локально с подом и монтированием | Кратковременные сетевые сбои (до 10–11 с) не приводят к выселению пода; при стойкой изоляции под удаляется через API, kubelet размонтирует том, узел переходит в `Secondary`, а контроллер пересоздаёт под на узле с кворумом |
| R34 | Автоматический `adjust` при `StandAlone`: пока локальная роль узла — `Secondary`, агент выполняет до 3 попыток `drbdadm adjust` с паузой 5 с между попытками. При роли `Primary` `adjust` не выполняется | На стенде (опыты E1, C2) после изоляции и освобождения тома `adjust` на обеих сторонах связи восстанавливал соединение за ~2 с без потери данных |
| R35 | Split-brain и метрика ручного вмешательства: если после 3 попыток `adjust` соединение остаётся `StandAlone`, агент прекращает попытки и выставляет метрику `ambar_volume_manual_check_needed{volume, node} = 1`. Метрика автоматически сбрасывается в `0`, как только статус соединения переходит в `Connected` | DRBD не отличает split-brain в статусе; предотвращается бесконечный цикл вызова утилит ядра; после вмешательства человека (`connect --discard-my-data`) метрика очищается сама |
| R36 | Уборка зависших записей `Client` без подов: если узел числится `Client` в `spec.nodes`, локальный ресурс закрыт/не используется (`open: false`, `Secondary`), и в API на узле нет подов с этим томом — агент снимает ресурс (`drbdadm down`) и удаляет свою запись `Client` из `spec.nodes` JSON-патчем | Защита от утечки клиентов при перезагрузке узлов или принудительном удалении подов во время `NodeUnstageVolume` |
| R37 | Безопасная проверка метаданных файлов без маркеров готовности (`losetup --read-only`): на пути удаления (`DeleteReplica`, `RetireReplica`), если файл тома существует, но маркеры `.img.ready` и `.img.synced` отсутствуют, агент подключает файл как `losetup --read-only`, выполняет `dump-md` и отключает loop. При `Absent` или `Fresh` удаляет копию, при `Used` удерживает (`Kept`) | Решение владельца от 2026-11-14. Предотвращает зависание копий после перезагрузки узла, подтверждая отсутствие полезных данных |
| R38 | Экспорт метрик Prometheus: агент запускает встроенный легковесный HTTP-эндпоинт на порту 9770 (`/metrics`) с набором из 7 метрик спецификации | Полная наблюдаемость состояния репликации и кворума томов кластера |

---

## Global Constraints

- Версия ядра: `6.12.116+deb13-arm64`. DRBD 9.3.4, drbd-utils 9.35.0. Версии крейтов закреплены в `Cargo.lock`.
- Правила безопасности данных фаз 1, 2а и 2б действуют без изменений.
- Никаких вызовов `drbdadm primary --force` со стороны агента.
- Вызовы API Kubernetes ограничены `API_CALL_TIMEOUT` (30 с) через `agent::kube::bounded`.
- Тесты лежат в `tests/` рядом с модулем (`#[cfg(test)] #[path = "tests/<name>.rs"] mod tests;`).
- TDD: сначала падающий тест, затем реализация; проверка мутацией для правил безопасности.
- Гейт перед каждым коммитом:
  `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked && image/test-loader.sh`
- Коммиты: строго локально в `main` через явные файлы `git commit -m "..." -- <files>`, без опций `-a`/`-A`, без трейлеров `Co-Authored-By`.
- Все диалоги, рассуждения и планы — на русском языке. Термины: «носитель» / «carrier» (не «карьер»), «гейт» / «gate» (не «ворота»).
- Двуязычная документация обновляется строго синхронно в парах `README.md` и `README.ru.md`.

---

## Структура файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `stand/spike/pre-3.sh` | опыты отказов и восстановления на стенде (F1–F4) | 1 |
| `deploy/rbac.yaml`, `deploy/daemonset.yaml`, `src/main.rs` | параметры CLI, права на nodes и pods, метрический порт | 2 |
| `src/backing.rs`, `src/drbd/adm.rs`, `src/agent/local.rs` | безопасная проверка метаданных через `losetup --read-only` | 3 |
| `src/agent/kube.rs`, `src/agent/taint.rs` (новый) | чистая логика и API установки/снятия taint `no-quorum` | 4 |
| `src/agent/quorum.rs` (новый), `src/agent/decide.rs` | отслеживание кворума, grace period, удаление подов | 5 |
| `src/agent/standalone.rs` (новый), `src/agent/run.rs` | авто-`adjust` при `StandAlone`, лимит 3 попыток | 6 |
| `src/agent/cleanup.rs` (новый), `src/agent/run.rs` | уборка зависших записей `Client` без подов | 7 |
| `src/metrics.rs` (новый), `src/agent/metrics_server.rs` (новый) | сборщик метрик и HTTP-сервер Prometheus (:9770/metrics) | 8 |
| `src/agent/run.rs`, `src/main.rs` | объединение всех механизмов в цикл агента | 9 |
| `stand/failure-e2e.sh` (новый), `stand/README.md` | сценарии сквозного тестирования отказов на стенде | 10 |
| `docs/superpowers/specs/**`, `README.md`, `README.ru.md` | документация и закрытие открытых вопросов | 11 |

---

### Task 1: Опыты на стенде перед кодом (F1–F4)

Подтверждение поведения DRBD 9 и Kubernetes на стенде для механизмов фазы 3:
- F1: изоляция узла с писателем (`iptables` блокировка TCP 7000) $\to$ `quorum: false` в статусе, EIO через 10–11 с; поведение при снятии.
- F2: `StandAlone` $\to$ вызов `drbdadm adjust` на обеих сторонах связи при роли `Secondary` возвращает `Connected`.
- F3: подключение файла тома без маркеров через `losetup --read-only` $\to$ `dump-md` возвращает `Absent` для чистого файла или `Fresh` для инициализированного без данных.
- F4: доступность порта 9770 с других узлов кластера.

**Files:**
- Create: `stand/spike/pre-3.sh`
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`
- Modify: `stand/README.md`

- [ ] **Step 1: подготовка стенда и скрипта `pre-3.sh`.** Написание сценариев F1–F4.
- [ ] **Step 2: прогон F1–F4 на стенде.** Запись логов и выводов.
- [ ] **Step 3: фиксация результатов в протоколе опытов.** Обновление `2026-11-12-drbd9-spike-results.md`.
- [ ] **Step 4: гейт и коммит.**

---

### Task 2: Параметры CLI, права RBAC и конфигурация подов

Добавление параметров агента `--quorum-loss-grace-period`, `--adjust-attempts`, `--adjust-interval`, `--metrics-addr`. Расширение `deploy/rbac.yaml` (права на `nodes` и `pods`) и открытие порта 9770 в `deploy/daemonset.yaml`.

**Files:**
- Modify: `src/main.rs`
- Modify: `deploy/rbac.yaml`
- Modify: `deploy/daemonset.yaml`
- Modify: `src/tests/` (тесты парсинга CLI аргументов)

- [x] **Step 1: тесты парсинга новых CLI параметров агента.**
- [x] **Step 2: реализация параметров в `src/main.rs`.**
- [x] **Step 3: обновление `deploy/rbac.yaml` и `deploy/daemonset.yaml`.**
- [x] **Step 4: гейт и коммит.**

---

### Task 3: Безопасная проверка метаданных через `losetup --read-only` (R37)

Реализация метода подключения файла тома в режиме только для чтения для безопасного вызова `dump-md` на пути удаления томов, у которых потеряны маркеры после перезагрузки.

**Files:**
- Modify: `src/backing.rs`
- Modify: `src/drbd/adm.rs`
- Modify: `src/agent/local.rs`
- Modify: `src/agent/run.rs`
- Create/Modify: тесты в `src/tests/`

- [x] **Step 1: тесты `attach_loop_read_only` и проверки `dump-md`.**
- [x] **Step 2: реализация read-only подключения в `backing.rs`.**
- [x] **Step 3: интеграция в `prove_no_data` и пути `DeleteReplica` / `RetireReplica`.**
- [x] **Step 4: гейт и коммит.**

---

### Task 4: Управление taint `ambar.balookrd.github.io/no-quorum` (R32)

Чистая логика расчёта необходимости taint и реализация установки/снятия taint через Kubernetes API.

**Files:**
- Create: `src/agent/taint.rs`
- Create: `src/agent/tests/taint.rs`
- Modify: `src/agent/kube.rs`
- Modify: `src/tests/volumes.rs` (фейк для taints)

- [x] **Step 1: unit-тесты чистой функции расчёта taint.**
- [x] **Step 2: методы `ensure_node_taint` и `remove_node_taint` в `KubeVolumes`.**
- [x] **Step 3: интеграция в фейки и проверка идемпотентности.**
- [x] **Step 4: гейт и коммит.**

---

### Task 5: Отслеживание кворума и освобождение тома (R33)

Реализация отслеживания состояния кворума смонтированных томов, ведение таймера grace period (30 с) и принудительное удаление подов при истечении таймаута через API.

**Files:**
- Create: `src/agent/quorum.rs`
- Create: `src/agent/tests/quorum.rs`
- Modify: `src/agent/kube.rs` (метод `delete_node_pods_for_volume`)
- Modify: `src/agent/run.rs`

- [x] **Step 1: unit-тесты таймера grace period и условий удаления подов.**
- [x] **Step 2: реализация метода поиска и удаления подов в `KubeVolumes`.**
- [x] **Step 3: интеграция проверок кворума в цикл агента.**
- [x] **Step 4: гейт и коммит.**

---

### Task 6: Автоматический `adjust` при `StandAlone` и метрика split-brain (R34, R35)

Реализация автоматического восстановления соединений `StandAlone` для узлов в роли `Secondary`: до 3 попыток `drbdadm adjust` с паузой 5 с, фиксация исчерпания попыток.

**Files:**
- Create: `src/agent/standalone.rs`
- Create: `src/agent/tests/standalone.rs`
- Modify: `src/agent/run.rs`

- [x] **Step 1: unit-тесты счётчика попыток `adjust` и фиксации состояния.**
- [x] **Step 2: реализация вызова `adjust` и перехода в `manual_check_needed`.**
- [x] **Step 3: автоматический сброс при переходе в `Connected`.**
- [x] **Step 4: гейт и коммит.**

---

### Task 7: Очистка зависших записей `Client` без подов (R36)

Реализация обнаружения записей `Client`, для которых на узле отсутствуют монтирующие поды, безопасный спуск ресурса (`down`) и удаление записи из `spec.nodes`.

**Files:**
- Create: `src/agent/cleanup.rs`
- Create: `src/agent/tests/cleanup.rs`
- Modify: `src/agent/run.rs`

- [x] **Step 1: unit-тесты условий очистки записей `Client`.**
- [x] **Step 2: реализация безопасного спуска и JSON-патча удаления.**
- [x] **Step 3: гейт и коммит.**

---

### Task 8: Метрики Prometheus и HTTP-эндпоинт :9770/metrics (R38)

Реализация сбора 7 метрик спецификации и асинхронного легковесного HTTP-сервера для их отдачи сборщику метрик Prometheus.

**Files:**
- Create: `src/metrics.rs`
- Create: `src/agent/metrics_server.rs`
- Create: `src/tests/metrics.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: unit-тесты генерации формата экспозиции Prometheus.**
- [ ] **Step 2: сбор метрик по ресурсам томов и состоянию узла.**
- [ ] **Step 3: реализация HTTP-сервера `:9770/metrics` на tokio.**
- [ ] **Step 4: гейт и коммит.**

---

### Task 9: Интеграция механизмов отказов в рабочий цикл агента

Объединение всех модулей (taints, quorum loss, auto-adjust, client cleanup, metrics server) в основной цикл `run_agent` и graceful shutdown.

**Files:**
- Modify: `src/agent/run.rs`
- Modify: `src/main.rs`
- Create: интеграционные тесты цикла агента

- [ ] **Step 1: тесты согласованной работы всех механизмов при отказах.**
- [ ] **Step 2: запуск фоновых задач и интеграция в `Agent::run`.**
- [ ] **Step 3: гейт и коммит.**

---

### Task 10: Сквозная проверка на стенде (`stand/failure-e2e.sh`)

Написание и двукратный успешный прогон комплексного сценария тестирования отказов на 4 узлах стенда:
- изоляция сети репликации и проверка taint;
- выселение пода по истечении 30 с и переезд на узел с кворумом;
- авто-восстановление `StandAlone` через `adjust`;
- фиксация split-brain метрики при сбое;
- очистка забытого `Client`;
- проверка эндпоинта `/metrics`.

**Files:**
- Create: `stand/failure-e2e.sh`
- Modify: `stand/README.md`
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`

- [ ] **Step 1: разработка скрипта `stand/failure-e2e.sh`.**
- [ ] **Step 2: прогон сквозного E2E-сценария на стенде.**
- [ ] **Step 3: протоколирование результатов и чистоты стенда.**
- [ ] **Step 4: гейт и коммит.**

---

### Task 11: Актуализация двуязычной документации

Синхронизация архитектурной спецификации, протокола опытов, двуязычных `README.md` и `README.ru.md`, закрытие открытых вопросов фазы 3.

**Files:**
- Modify: `docs/superpowers/specs/2026-11-12-ambar-design.md`
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `docs/superpowers/plans/2026-11-18-ambar-phase3-failure-handling.md`

- [ ] **Step 1: актуализация спецификации дизайна.**
- [ ] **Step 2: синхронное обновление `README.md` и `README.ru.md`.**
- [ ] **Step 3: проверка стиля, приватных данных и финальный гейт.**
- [ ] **Step 4: коммит.**
