# ambar, фаза 2а: Kubernetes (CRD, цикл согласования, CSI) — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** PVC со `StorageClass` драйвера `ambar.balookrd.github.io` создаёт том с N копиями, под монтирует его на любом узле кластера, удаление PVC удаляет том и его данные на узлах.

**Архитектура:** тот же бинарь `ambar` получает команду `agent`: в одном процессе цикл согласования по объектам `ReplicatedVolume` и gRPC-сервер CSI (Identity, Controller, Node) на unix-сокете. Решения «что делать» остаются чистыми функциями (`placement`, `membership`, `agent::decide`), которые тестируются без кластера; работа с API Kubernetes, gRPC и внешними командами отделена от них. Действия на узле выполняет `reconcile::Reconciler` из фазы 1.

**Стек:** Rust (edition 2024), `tokio`, `kube` и `k8s-openapi`, `schemars`, `tonic` и `prost`, CSI spec v1; сайдкары `external-provisioner` и `node-driver-registrar`; DRBD 9.3.4, drbd-utils 9.35.0; стенд Lima с k3s.

**Спека:** `docs/superpowers/specs/2026-11-12-ambar-design.md`. **Протокол опытов:** `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`. **Стенд:** `stand/README.md`. **План фазы 1 (исполнен):** `docs/superpowers/plans/2026-11-13-ambar-phase1-agent-core.md`.

## Место этой фазы

| Фаза | Что даёт | Состояние |
|---|---|---|
| 0. Образ и стенд | модуль DRBD 9 в образе, стенд из ВМ Lima | исполнена |
| 1. Ядро агента | локальное согласование одного тома через CLI | исполнена |
| 2а. Kubernetes (этот план) | CRD, цикл согласования, CSI, манифесты; том через PVC на стенде | план написан |
| 2б. Смена набора копий | добавление, удаление и перенос копии у существующего тома | план не написан |
| 3. Отказы | освобождение тома при потере кворума, taint, `adjust` при `StandAlone`, метрики | план не написан |
| 4. Замер на R5C | память, `fsync`, сравнение с Longhorn | нужны узлы |

Что в этой фазе не делается: смена набора копий (API её отклоняет), освобождение тома при потере кворума, taint, обработка `StandAlone`, метрики, расширение тома, снапшоты, уборка членства `Client` без пода после перезагрузки узла.

## Решения этого плана

Решения владельца от 2026-11-13 записаны в спеке («Принятые решения»). Ниже технические решения, принятые при составлении плана; каждое меняет или уточняет спеку, задача 15 переносит их в неё.

| № | Решение | Причина |
|---|---|---|
| R1 | Запись узла в `spec.nodes` имеет поле `kind`: `Replica` (копия данных), `Witness` (постоянный узел без диска при чётном N), `Client` (узел без диска, подключённый на время работы пода). Поле `diskful` из спеки заменено им | Свидетеля нельзя отключать при уходе пода, клиента нужно; по одному `diskful: false` их не различить |
| R2 | `address` и `maxPeers` записаны в `ReplicatedVolume`: адрес копируется из `ConfigMap ambar-nodes` при добавлении узла в том, `maxPeers` задаётся при создании и не меняется | Агент собирает описание тома из одного объекта; закрывает строку «Открытых вопросов» |
| R3 | `maxPeers` — параметр `StorageClass`, по умолчанию `7` | `create-md --max-peers=7` проверен на стенде (опыты P2, P5); другие значения не запускались |
| R4 | `nodeId` узла постоянный и берётся из `ConfigMap ambar-nodes`; узел без записи в ней не может быть участником тома | `nodeId` стороны DRBD нельзя менять у работающего ресурса; один источник номеров на кластер |
| R5 | `ConfigMap ambar-nodes` хранит список в ключе `nodes` как JSON-массив | В крейте есть `serde_json`, YAML-парсер не нужен; JSON-массив является и корректным YAML |
| R6 | `minor` и `port` выделяются одним индексом: `minor = 1000 + i`, `port = 7000 + i`, `i` — наименьший свободный среди существующих объектов. Агент перед применением проверяет, что `minor` и `port` тома не совпадают с другим объектом, и отказывается применять том с совпадением | Закрывает вопрос ревью о коллизиях |
| R7 | Отметка «том новый»: поле `spec.initialSync` со значениями `Skip` и `Done`. `CreateVolume` создаёт объект со `Skip`; агент-лидер после записи маркера `.img.synced` меняет значение на `Done`. Отсутствующее поле читается как `Done` | Решение владельца; значение по умолчанию запрещает пропуск синхронизации |
| R8 | Членство узла в томе при `NodeStageVolume` добавляет агент этого узла: дописывает себя в `spec.nodes` с `kind: Client`. При `NodeUnstageVolume` агент удаляет свою запись `Client` | `attachRequired: false`, вызова `ControllerPublishVolume` нет |
| R9 | Финализаторы — по одному на узел с копией: `ambar.balookrd.github.io/replica-<имя узла>`. Агент узла снимает свой финализатор после удаления локальной копии. Единого финализатора `…/cleanup` и ожидания пустого `status.nodes` нет | Не нужен отдельный контроллер с выбором лидера; объект исчезает, когда все копии удалены |
| R10 | Агент удаляет локальные данные тома только в одном случае: объект существует, у него стоит `deletionTimestamp`, и на объекте есть финализатор этого узла. Файл тома без объекта или без членства узла агент не трогает и пишет ошибку в журнал | Ошибочная правка `spec.nodes` или удаление объекта в обход финализатора не должны стирать данные |
| R11 | Остатки узла без диска (файл ресурса и ресурс в ядре без файла тома) агент убирает, если узел не участник тома или объекта нет | У узла без диска нет данных; так убирается членство `Client` после сбоя между правкой объекта и локальной уборкой |
| R12 | `status.nodes` — список с ключом `name`; каждый агент пишет только свою запись через server-side apply с менеджером полей `ambar-<узел>` | Параллельные записи разных узлов не затирают друг друга |
| R13 | Каталог томов агент не создаёт: `hostPath` с `type: Directory`, `lock_volume` возвращает ошибку, если каталога нет | Если раздел с томами не смонтирован, агент не должен создать пустые тома на корневой файловой системе |
| R14 | Внешние команды выполняются с тайм-аутом 120 с; по истечении процесс получает `SIGKILL`, вызов возвращает ошибку. Действия на узле идут в `spawn_blocking` | Зависшая команда не должна останавливать цикл согласования и gRPC |
| R15 | Идентификатор объекта в маркере `.img.ready` не записывается | Имя тома — `pvc-<UUID>` от `external-provisioner`; при R9 и R10 повторное использование имени с остатками прежнего тома требует ручного вмешательства в обход финализатора |
| R16 | Топология: драйвер объявляет `VOLUME_ACCESSIBILITY_CONSTRAINTS`, `NodeGetInfo` возвращает сегмент `ambar.balookrd.github.io/node=<имя узла>`, `CreateVolume` берёт выбранный планировщиком узел из первого элемента `preferred`. В ответе `CreateVolume` поля `accessible_topology` нет | Так драйвер узнаёт `selected-node`; PV без `nodeAffinity` монтируется на любом узле |
| R18 | Перед удалением локальной копии агент читает локальный статус DRBD: если роль узла `Primary` или у любой стороны `peer-role` равен `Primary`, копия не удаляется, в статус пишется причина, попытка повторяется | Объект, удалённый вручную при работающем поде, не должен лишиться копий на остальных узлах, пока том смонтирован |
| R17 | Четвёртая ВМ стенда `dv-4` | В кластере из трёх узлов `kind: Client` не возникает: при `replicas: 3` все узлы с копией, при `replicas: 2` третий узел — свидетель |

Следствие R17 для кластера владельца: пока в кластере три узла, подключение по требованию не срабатывает ни для одного тома; оно начнёт работать с четвёртого узла.

## Global Constraints

- Версия ядра: `6.12.116+deb13-arm64`. DRBD 9.3.4, drbd-utils 9.35.0. Версии не менять.
- Правила безопасности данных фазы 1 действуют без изменений (шапка `src/reconcile.rs`): `create-md --force` только без маркера `.img.ready`; `new-current-uuid --clear-bitmap` только лидером при выполненном гейте и без маркера `.img.synced`; `drbdadm primary --force` не вызывается; результат команд проверяется повторным чтением статуса.
- Новые правила этой фазы: R10 и R18 (когда удаляются данные), R7 (пропуск синхронизации только при `initialSync: Skip`), проверка `dump-md` перед `create-md`, `mkfs` только при одновременном выполнении трёх условий задачи 9.
- Набор узлов с `kind: Replica` и `kind: Witness`, а также `sizeBytes`, `minor`, `port`, `maxPeers`, `protocol` у существующего объекта не меняются. Запрет обеспечивают правила валидации CRD (задача 3); план 2б ослабит их для набора копий.
- Группа API: `ambar.balookrd.github.io`, версия `v1alpha1`, вид `ReplicatedVolume`, cluster-scoped. Имя драйвера CSI: `ambar.balookrd.github.io`. Пространство имён: `ambar-system`.
- Пути на узле: тома `/var/lib/rancher/ambar/volumes`, файлы ресурсов `/var/lib/rancher/ambar/res` (в контейнере `/etc/drbd.d`), сокет CSI `/var/lib/kubelet/plugins/ambar/csi.sock`.
- Имя узла Kubernetes равно `uname -n` узла. Агент при старте сравнивает переменную окружения `NODE_NAME` с `/proc/sys/kernel/hostname` и завершается с ошибкой при расхождении.
- Версии крейтов и образов сайдкаров исполнитель берёт последние стабильные на день исполнения, фиксирует (`Cargo.lock`, digest образа) и записывает в отчёт задачи. Номера версий в этом плане не заданы намеренно. Перед использованием API крейта исполнитель сверяется с его документацией для зафиксированной версии.
- `rust-version` в `Cargo.toml` поднимается до минимума, которого требуют новые зависимости; вместе с ним меняются задача `msrv` в `.github/workflows/ci.yml` и строка требований в `README.md` и `README.ru.md`.
- Тесты лежат в `tests/` рядом с модулем и подключаются через `#[cfg(test)] #[path = "tests/<имя>.rs"] mod tests;`. Inline-блоков `mod tests { … }` нет.
- Гейт перед каждым коммитом: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `image/test-loader.sh`. Linux-специфичный код (монтирование, `mountinfo`) до коммита проверяется в контейнере: `docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`.
- Комментарии в коде и сообщения коммитов — на английском. Документы в `docs/superpowers/**` — на русском. `README.md` и `README.ru.md` правятся вместе.
- Репозиторий публичный. В файлы и сообщения коммитов не попадают адреса домашней сети владельца, его домашний домен и путь домашнего каталога; адреса стенда `192.168.104.x` и адреса из `192.0.2.0/24` в примерах допустимы.
- `git commit -- <пути>` локально в `main`, без веток, без трейлера `Co-Authored-By` и подписей об авторстве. `git push` — только по отдельной команде владельца.
- Утверждение в документах о поведении DRBD, `blkid`, `mount` или Kubernetes допускается только со ссылкой на опыт из протокола либо с пометкой `[проверить: …]`.

## Структура файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `stand/spike/pre-csi.sh` | опыты P7–P9 | 1 |
| `src/exec.rs` | тайм-аут команд | 2 |
| `src/model.rs`, `src/reconcile.rs`, `src/drbd/adm.rs` | `initialSync`, проверка `dump-md`, каталог томов | 2 |
| `src/crd.rs` | тип `ReplicatedVolume`, преобразование в `VolumeSpec` | 3 |
| `deploy/crd.json` | CRD, сгенерирован командой `ambar crd` | 3 |
| `src/config.rs` | `ConfigMap ambar-nodes` | 4 |
| `src/placement.rs` | выбор узлов, индекс `minor`/`port`, бюджет | 5 |
| `src/membership.rs` | добавление и удаление `Client`, имена финализаторов, коллизии | 6 |
| `src/agent/decide.rs` | решение агента по одному объекту | 7 |
| `src/agent/status.rs` | запись узла в `status.nodes` из статуса DRBD | 7 |
| `src/agent/run.rs`, `src/agent/kube.rs` | цикл согласования, вызовы API | 8 |
| `src/mount.rs` | `mountinfo`, признак файловой системы, `mkfs`, `mount`, `umount` | 9 |
| `proto/csi.proto`, `build.rs`, `src/csi/mod.rs`, `src/csi/identity.rs` | типы CSI, сервер, Identity | 10 |
| `src/csi/controller.rs` | `CreateVolume`, `DeleteVolume` | 11 |
| `src/csi/node.rs` | `NodeStageVolume` и остальные вызовы Node | 12 |
| `deploy/*.yaml`, `image/Dockerfile`, `stand/install.sh` | манифесты и установка на стенд | 13 |
| `stand/lib.sh`, `stand/csi-e2e.sh` | четвёртая ВМ, сквозная проверка | 14 |
| спека, `README.md`, `README.ru.md`, `stand/README.md`, CI | документы | 15 |

`src/main.rs` получает команды `agent` и `crd`; команды фазы 1 остаются.

---

### Task 1: опыты на стенде перед кодом (P7–P9)

Кода агента в задаче нет. Результаты определяют строки и коды возврата, на которые опираются задачи 2 и 9, и порядок действий задач 8 и 12.

**Files:**
- Create: `stand/spike/pre-csi.sh` (по образцу `stand/spike/pre-agent.sh`: `source ../lib.sh`, функции `cleanup`, `write_res`, `backing`, `run_rc`, выбор опыта аргументом)
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` (новый раздел «Опыты перед CSI (P7–P9)»)
- Modify: `stand/README.md` (строка о скрипте)

**Interfaces:**
- Produces: записанные в протоколе (а) код возврата и текст `drbdadm dump-md` для трёх случаев; (б) порядок команд отключения стороны без диска и наблюдения за записью; (в) код возврата и вывод `blkid -p` для четырёх случаев. Задачи 2, 8, 9, 12 ссылаются на эти записи.

- [ ] **Step 1: P7, `drbdadm dump-md`.** На `dv-1`, ресурс `p7` (minor 1007, port 7007), файл 1 ГиБ, ресурс не поднят. Записать код возврата, stdout (первые 20 строк) и stderr команды `drbdadm dump-md p7` в трёх состояниях: (1) пустой разреженный файл без метаданных; (2) после `drbdadm create-md --force --max-peers=7 p7`; (3) после `drbdadm up p7` (ресурс настроен, диск подключён). Команды запускаются через `tb dv-1`.

Ожидается (предположение, проверяется опытом): (1) ненулевой код и текст о том, что метаданных нет; (2) код 0 и дамп; (3) отказ, потому что устройство занято.

- [ ] **Step 2: P8, отключение стороны без диска от работающего ресурса.** Ресурс `p8` (minor 1008, port 7008): `dv-1` и `dv-2` с копией, `dv-3` без диска, `--max-peers=7`, начальная синхронизация пропущена, на `dv-1` смонтирована ext4 и работают `probe.sh` и `watch.sh` (как в P5 из `pre-agent.sh`). Затем:
  1. На `dv-3`: `drbdadm down p8`, удалить `p8.res`. Записать `drbdsetup status p8 --verbose` на `dv-1` и `dv-2` (ожидается соединение с `dv-3` в `Connecting`).
  2. Переписать `p8.res` на `dv-1` и `dv-2` без секции `dv-3` и без него в `connection-mesh`; выполнить `drbdadm adjust p8` на обоих; записать код возврата, вывод и `drbdsetup status p8 --verbose`.
  3. Записать хвост журнала `watch.sh`, `dmesg -T | grep 'drbd p8'` и `quorum` из статуса.
  4. Вернуть `dv-3` (файл ресурса с тремя сторонами на всех, `adjust` на `dv-1`, `dv-2`, `up` на `dv-3`), записать статус и размер битовой карты: `drbdsetup status p8 --verbose --statistics`.
  5. Остановить писателя, сравнить подтверждённый номер и номер на томе.

Вопросы, на которые отвечает запись: исчезает ли соединение из статуса после `adjust` без стороны в файле; прерывалась ли запись; менялся ли `quorum`; подключается ли сторона повторно с тем же `node-id`.

- [ ] **Step 3: P9, признак файловой системы и занятое устройство.** Ресурс `p9` (minor 1009, port 7009) на трёх узлах с копией, синхронизация пропущена. Записать код возврата, stdout и stderr команды `blkid -p -o value -s TYPE /dev/drbd1009` на `dv-2` в четырёх состояниях: (1) устройство без файловой системы; (2) после `mkfs.ext4 -q` на `dv-1`, том не смонтирован; (3) том смонтирован на `dv-1` (на `dv-2` открытие должно дать `EMEDIUMTYPE`); (4) устройства `/dev/drbd1999` не существует. Для тех же состояний записать результат чтения двух байт по смещению 1080 от имени агента: `dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none | od -An -tx1` (у ext4 — `53 ef`).

- [ ] **Step 4: записать результаты.** В протокол добавить раздел «Опыты перед CSI (P7–P9)»: таблица «опыт | условия | команда | код возврата | вывод | число прогонов», под ней сырой вывод в блоках кода. Вывод о поведении формулируется в пределах записанного; число прогонов указано. Адреса в выводе — только `192.168.104.x`.

- [ ] **Step 5: проверить чистоту стенда и закоммитить.**

Run: `stand/spike/pre-csi.sh clean && stand/check.sh`
Expected: `stand/check.sh` завершается с кодом 0; `tb dv-1 "drbdsetup status"` не печатает ресурсов `p7`, `p8`, `p9`.

```bash
git add stand/spike/pre-csi.sh
git commit -m "docs(spike): pre-CSI checks for dump-md, peer removal and filesystem detection" -- stand/spike/pre-csi.sh stand/README.md docs/superpowers/specs/2026-11-12-drbd9-spike-results.md
```

Если результат P7 или P9 не совпал с ожиданием либо в P8 `adjust` не убрал соединение с ушедшей стороной или запись прервалась, исполнитель останавливается и сообщает контроллеру: от этих результатов зависят условия в задачах 2, 9 и 12.

---

### Task 2: изменения ядра агента (`initialSync`, `dump-md`, тайм-аут, каталог томов)

**Files:**
- Modify: `src/model.rs`, `src/tests/model.rs`
- Modify: `src/reconcile.rs`, `src/tests/reconcile.rs`
- Modify: `src/drbd/adm.rs`, `src/drbd/tests/adm.rs`
- Modify: `src/exec.rs`, `src/tests/exec.rs`
- Modify: `src/main.rs`
- Modify: `stand/agent-smoke.sh`, `stand/k3s.sh`

**Interfaces:**
- Consumes: запись P7 из задачи 1 (код возврата и текст `dump-md` для файла без метаданных).
- Produces:

```rust
// src/model.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum InitialSync {
    /// The volume was just created and holds no data: the leader may declare
    /// the copies identical without copying.
    Skip,
    /// The copies must never be declared identical without copying.
    #[default]
    Done,
}
// VolumeSpec gets: #[serde(default)] pub initial_sync: InitialSync,

// src/exec.rs
pub struct SystemRunner {
    /// A command still running after this long is killed and reported as an error.
    pub timeout: Duration,
}
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
impl Default for SystemRunner { /* timeout: DEFAULT_COMMAND_TIMEOUT */ }

// src/drbd/adm.rs
pub enum Metadata { Present, Absent }
impl Drbd<'_> { pub fn metadata(&self, volume: &str) -> Result<Metadata, DrbdError>; }
// DrbdError gets: MetadataCheck { volume: String, detail: String }

// src/reconcile.rs
// ReconcileError gets: MetadataPresent { volume: String }
//   message: "volume {volume:?}: the backing file already holds DRBD metadata but has no ready marker; \
//             nothing was changed. Check the file by hand before restoring the marker"
```

`JsonSchema` на `InitialSync` и на `Protocol` появляется в задаче 3 вместе с зависимостью `schemars`; в этой задаче derive без него.

- [ ] **Step 1: тесты `initialSync` (падают).** В `src/tests/model.rs`: описание без поля `initialSync` читается как `Done`; `"initialSync":"Skip"` читается как `Skip`; `"initialSync":"skip"` отклоняется. В `src/tests/reconcile.rs`: существующие тесты, которые ожидают `SkipInitialSync`, получают описание со `Skip`; новый тест `skip_is_never_planned_when_initial_sync_is_done` — вход, при котором сейчас планируется `[SkipInitialSync]`, со значением `Done` даёт пустой план (и `MarkSynced` не планируется, пока диск `Inconsistent`). В переборный тест инвариантов добавить измерение `initial_sync ∈ {Skip, Done}` и инвариант: `SkipInitialSync` ⇒ `initial_sync == Skip`.

Run: `cargo test --locked reconcile model`
Expected: новые тесты не компилируются или падают.

- [ ] **Step 2: реализовать.** Поле в `VolumeSpec`; в `plan` условие лидера дополняется `spec.initial_sync == InitialSync::Skip`; строка таблицы решений в шапке `src/reconcile.rs` дополняется этим условием. Временно убрать условие, убедиться, что новый тест падает, вернуть.

Run: `cargo test --locked`
Expected: все тесты проходят.

- [ ] **Step 3: тесты проверки метаданных (падают).** `src/drbd/tests/adm.rs`, с поддельным `Runner`: `drbdadm dump-md <том>` с кодом 0 → `Metadata::Present`; код и stderr, записанные в P7 для файла без метаданных → `Metadata::Absent`; любой другой ненулевой результат (например, код 20 и текст `Device is configured`) → `DrbdError::MetadataCheck`. Строка-признак берётся из протокола P7 дословно и хранится константой `NO_METADATA_MARKER` с комментарием-ссылкой на опыт. `src/tests/reconcile.rs`: исполнитель при `CreateMd` и `Metadata::Present` возвращает `MetadataPresent`, `create-md` не вызывается, маркер `.img.ready` не пишется; при `Absent` порядок команд `dump-md`, затем `create-md`; при `MetadataCheck` `create-md` не вызывается.

- [ ] **Step 4: реализовать `Drbd::metadata` и вызов в `Action::CreateMd`.** Проверка выполняется непосредственно перед `drbd.create_md` в `execute`; `plan` не меняется.

Run: `cargo test --locked`
Expected: все тесты проходят.

- [ ] **Step 5: тест тайм-аута (падает).** `src/tests/exec.rs`: `SystemRunner { timeout: 200 ms }.run("sleep", &["5"])` возвращает ошибку вида `io::ErrorKind::TimedOut` быстрее чем за 2 с; `run("sh", &["-c", "echo out; echo err >&2; exit 3"])` с тайм-аутом 5 с возвращает код 3, `stdout == "out\n"`, `stderr == "err\n"`; команда, которая пишет 1 МиБ в stdout, не зависает (каналы читаются, пока процесс работает).

- [ ] **Step 6: реализовать тайм-аут.** `Command::spawn` с каналами; два потока читают stdout и stderr до конца; основной поток опрашивает `try_wait` каждые 20 мс; по истечении `timeout` — `kill()`, `wait()`, ошибка `TimedOut` с текстом командной строки (`command_line`). `SystemRunner` в `src/main.rs` создаётся через `SystemRunner::default()`.

- [ ] **Step 7: каталог томов (R13).** Тест в `src/tests/reconcile.rs`: `apply` при отсутствующем `volumes_dir` возвращает `ReconcileError::Io` с действием `open`, каталог не появляется. Убрать `DirBuilder…create(&self.paths.volumes_dir)` из `lock_volume`. Существующие тесты, которые полагались на создание каталога, создают его сами. `stand/k3s.sh`: создавать `/var/lib/rancher/ambar/volumes` (режим 0700) и `/var/lib/rancher/ambar/res` на каждом узле явной командой.

- [ ] **Step 8: стенд.** В `stand/agent-smoke.sh` в описание тома добавить `"initialSync": "Skip"`. Пересобрать образ и прогнать проверку.

Run: `image/build.sh && stand/k3s.sh && stand/agent-smoke.sh`
Expected: последняя строка `OK: agent smoke`.

- [ ] **Step 9: коммит.**

```bash
git commit -m "feat(agent): initialSync gate, metadata check before create-md, command timeout; never create the volumes directory" -- src stand/agent-smoke.sh stand/k3s.sh
```

---

### Task 3: тип `ReplicatedVolume` и CRD

**Files:**
- Modify: `Cargo.toml` (зависимости `kube` с derive и клиентом, `k8s-openapi`, `schemars`, `tokio`)
- Create: `src/crd.rs`, `src/tests/crd.rs`
- Create: `deploy/crd.json`
- Modify: `src/lib.rs`, `src/main.rs` (команда `crd`)

**Interfaces:**
- Consumes: `model::{VolumeSpec, VolumeNode, Protocol, InitialSync}`.
- Produces:

```rust
// src/crd.rs
pub const GROUP: &str = "ambar.balookrd.github.io";
pub const DRIVER_NAME: &str = "ambar.balookrd.github.io";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NodeKind { Replica, Witness, Client }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberNode {
    pub name: String,
    pub node_id: u32,
    pub address: String,   // validated as IpAddr in to_volume_spec
    pub kind: NodeKind,
}

#[derive(CustomResource, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(group = "ambar.balookrd.github.io", version = "v1alpha1", kind = "ReplicatedVolume",
       status = "ReplicatedVolumeStatus", shortname = "rv")]   // cluster-scoped: no `namespaced`
#[serde(rename_all = "camelCase")]
pub struct ReplicatedVolumeSpec {
    pub size_bytes: u64,
    pub protocol: Protocol,
    pub minor: u32,
    pub port: u16,
    pub max_peers: u32,
    #[serde(default)]
    pub initial_sync: InitialSync,
    pub nodes: Vec<MemberNode>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReplicatedVolumeStatus {
    #[serde(default)]
    pub nodes: Vec<NodeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub name: String,
    pub role: String,          // "Primary" | "Secondary" | "Unknown"
    pub disk: String,          // DRBD disk state of the local volume 0, "Diskless" for a node without a disk
    pub quorum: bool,
    pub connections: Vec<PeerStatus>,
    /// Last error of the reconcile on this node; empty when the last pass succeeded.
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus { pub peer: String, pub state: String, pub peer_disk: String }

/// Description the node agent of phase 1 works with.
pub fn to_volume_spec(name: &str, spec: &ReplicatedVolumeSpec) -> Result<VolumeSpec, CrdError>;
pub enum CrdError { Address { node: String, address: String }, Spec(SpecError) }
```

`to_volume_spec`: `diskful = kind == Replica`; адрес разбирается как `IpAddr`; результат проходит `VolumeSpec::validate`.

Схема CRD: `status.nodes` — `x-kubernetes-list-type: map` с ключом `name` (нужно для R12); `spec.nodes` — то же. Правила `x-kubernetes-validations` на `spec` (CEL, переходные):

| Правило | Сообщение |
|---|---|
| `self.sizeBytes == oldSelf.sizeBytes && self.minor == oldSelf.minor && self.port == oldSelf.port && self.maxPeers == oldSelf.maxPeers && self.protocol == oldSelf.protocol` | `sizeBytes, minor, port, maxPeers and protocol are immutable` |
| набор записей с `kind != 'Client'` в `self.nodes` равен набору в `oldSelf.nodes` (сравнение по `name`, `nodeId`, `address`, `kind`) | `the set of Replica and Witness nodes is immutable` |
| у записи, присутствующей в обоих списках, `nodeId`, `address` и `kind` не изменились | `a member node cannot change nodeId, address or kind` |
| `!(oldSelf.initialSync == 'Done' && self.initialSync == 'Skip')` | `initialSync cannot go back to Skip` |

В схеме у `spec.initialSync` значение по умолчанию `Done`. Как передать `x-kubernetes-list-type`, значение по умолчанию и CEL-правила через `schemars`/`kube` в зафиксированной версии, исполнитель берёт из документации `kube` (атрибуты `#[kube(validation = …)]`, `#[schemars(extend(…))]` либо правка схемы функцией); способ записывается в отчёт.

- [ ] **Step 1: тесты (падают).** `src/tests/crd.rs`:
  - `to_volume_spec` для примера из трёх узлов (два `Replica`, один `Witness`) даёт `VolumeSpec` с `diskful` `[true, true, false]`, `max_peers`, `initial_sync` из объекта;
  - `Client` → `diskful: false`;
  - адрес `"not-an-ip"` → `CrdError::Address`;
  - один `Replica` → `CrdError::Spec(SpecError::Replicas(1))`;
  - объект без `initialSync` в JSON читается как `Done`;
  - `deploy/crd.json` побайтно равен выводу генератора (`serde_json::to_string_pretty(&ReplicatedVolume::crd())` плюс перевод строки);
  - в сгенерированной схеме: `scope == "Cluster"`, у `status` включён subresource, у `spec.nodes` и `status.nodes` стоит `x-kubernetes-list-map-keys: ["name"]`, в `spec` четыре правила `x-kubernetes-validations`.

- [ ] **Step 2: реализовать `src/crd.rs` и команду `ambar crd`** (печатает CRD в JSON в stdout). Сгенерировать `deploy/crd.json`.

Run: `cargo run --quiet -- crd > deploy/crd.json && cargo test --locked crd`
Expected: тесты проходят.

- [ ] **Step 3: проверить правила на стенде.**

Run: `kc apply -f - < deploy/crd.json` (через `stand/lib.sh`), затем создать объект `rv-test` из примера и попытаться: изменить `sizeBytes`; удалить запись `Replica`; добавить запись `Client`; удалить запись `Client`; сменить `initialSync` с `Done` на `Skip`.
Expected: добавление и удаление `Client` принимаются, остальные правки отклоняются с сообщениями из таблицы. Объект и CRD после проверки удалить. Вывод команд — в отчёт задачи.

- [ ] **Step 4: гейт и коммит.** Проверить `cargo zigbuild --release --locked --target aarch64-unknown-linux-musl`: бинарь с клиентом `kube` собирается под musl. Если TLS-бэкенд не собирается под musl, исполнитель выбирает другой бэкенд из поддерживаемых `kube` и записывает выбор и причину.

```bash
git add src/crd.rs src/tests/crd.rs deploy/crd.json
git commit -m "feat(crd): ReplicatedVolume type, immutability rules and the generated CRD" -- Cargo.toml Cargo.lock src deploy/crd.json
```

---

### Task 4: `ConfigMap ambar-nodes`

**Files:**
- Create: `src/config.rs`, `src/tests/config.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:

```rust
pub const NODES_CONFIGMAP: &str = "ambar-nodes";
pub const NODES_KEY: &str = "nodes";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClusterNode {
    pub name: String,
    pub address: IpAddr,
    pub node_id: u32,
    pub capacity_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNodes(Vec<ClusterNode>);   // validated, sorted by name

impl ClusterNodes {
    pub fn from_json(json: &str) -> Result<Self, ConfigError>;
    pub fn get(&self, name: &str) -> Option<&ClusterNode>;
    pub fn iter(&self) -> impl Iterator<Item = &ClusterNode>;
    pub fn len(&self) -> usize;
}
pub enum ConfigError { Json(String), NodeName(String), DuplicateName(String), NodeId(u32), DuplicateNodeId(u32), DuplicateAddress(IpAddr), Empty }
```

Правила: имя по `model::valid_node_name` (сделать `pub`); `nodeId ≤ model::MAX_NODE_ID`; имена, `nodeId` и адреса уникальны; список не пуст.

- [ ] **Step 1: тесты (падают).** По одному тесту на каждую ошибку `ConfigError`; корректный список из трёх узлов; порядок `iter()` по имени независимо от порядка во входе; неизвестное поле отклоняется.
- [ ] **Step 2: реализовать.**

Run: `cargo test --locked config`
Expected: тесты проходят.

- [ ] **Step 3: коммит.**

```bash
git add src/config.rs src/tests/config.rs
git commit -m "feat(config): parse and validate the ambar-nodes ConfigMap" -- src
```

---

### Task 5: размещение тома (`placement`)

**Files:**
- Create: `src/placement.rs`, `src/tests/placement.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `config::ClusterNodes`, `crd::{ReplicatedVolumeSpec, MemberNode, NodeKind}`, `model::{Protocol, InitialSync}`.
- Produces:

```rust
pub const FIRST_MINOR: u32 = 1000;
pub const FIRST_PORT: u16 = 7000;
pub const DEFAULT_REPLICAS: usize = 3;
pub const DEFAULT_MAX_PEERS: u32 = 7;
pub const DEFAULT_SIZE_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub size_bytes: u64,          // already rounded up to a multiple of 4096
    pub replicas: usize,
    pub protocol: Protocol,
    pub max_peers: u32,
    /// Node the scheduler picked for the first pod, when known.
    pub selected_node: Option<String>,
}

/// What existing volumes already occupy: (minor, port, replica node names, size).
pub struct Existing { pub minor: u32, pub port: u16, pub replica_nodes: Vec<String>, pub size_bytes: u64 }

pub fn place(req: &Request, nodes: &ClusterNodes, existing: &[Existing]) -> Result<ReplicatedVolumeSpec, PlaceError>;

pub enum PlaceError {
    Replicas(usize),                               // < 2
    NotEnoughNodes { replicas: usize, nodes: usize },
    UnknownSelectedNode(String),
    MaxPeers { max_peers: u32, needed: u32 },
    NoCapacity { node: String, free: u64, needed: u64 },
    NoFreeIndex,
}

/// Parameters of a StorageClass -> Request fields. Unknown keys are an error,
/// except keys starting with "csi.storage.k8s.io/".
pub fn parse_parameters(params: &BTreeMap<String, String>) -> Result<(usize, Protocol, u32), ParamError>;
pub fn round_up_size(required_bytes: i64) -> Result<u64, ParamError>;   // 0 -> DEFAULT_SIZE_BYTES; negative or > MAX_SIZE_BYTES -> error
```

Правила `place`:

1. `replicas < 2` → `Replicas`; `replicas > nodes.len()` → `NotEnoughNodes`.
2. Свободный бюджет узла = `capacity_bytes` минус сумма `size_bytes` существующих томов, где узел в `replica_nodes` (с насыщением до нуля).
3. Узлы с копией: `selected_node`, если задан (нет в `nodes` → `UnknownSelectedNode`), затем узлы по убыванию свободного бюджета, при равенстве по имени. У каждого выбранного свободный бюджет не меньше `size_bytes`, иначе `NoCapacity` для первого узла, которому не хватило. Узел, которому не хватает бюджета, не заменяется следующим только в случае `selected_node`; среди остальных берутся первые `replicas − 1` с достаточным бюджетом, и `NoCapacity` возвращается, если таких меньше.
4. Свидетель: при чётном `replicas` — узел без копии с наибольшим свободным бюджетом (при равенстве по имени); если узлов без копии нет, свидетеля нет.
5. `maxPeers` не меньше `nodes.len() − 1` и не больше `MAX_NODE_ID`, иначе `MaxPeers`. (Запас рассчитан на подключение любого узла кластера как `Client`.)
6. Индекс `i` — наименьший, для которого ни `1000 + i`, ни `7000 + i` не заняты в `existing`; `7000 + i > 65535` или `1000 + i > MAX_MINOR` → `NoFreeIndex`.
7. Результат: `initial_sync: Skip`; `nodes` отсортированы по имени; `nodeId` и `address` из `ClusterNodes`.

- [ ] **Step 1: тесты (падают).** Таблица случаев, по тесту на строку:

| Случай | Ожидание |
|---|---|
| 3 узла, `replicas: 3`, нет томов | три `Replica`, свидетеля нет, `minor 1000`, `port 7000`, `Skip` |
| 3 узла, `replicas: 2`, `selected_node` = `n2` | `n2` и узел с наибольшим бюджетом из остальных — `Replica`, третий — `Witness` |
| 4 узла, `replicas: 2` | два `Replica`, один `Witness`, четвёртого узла в `nodes` нет |
| 2 узла, `replicas: 2` | два `Replica`, свидетеля нет |
| 4 узла, `replicas: 4` | четыре `Replica`, свидетеля нет |
| `replicas: 1` | `Replicas(1)` |
| `replicas: 4`, 3 узла | `NotEnoughNodes` |
| `selected_node` неизвестен | `UnknownSelectedNode` |
| у `selected_node` не хватает бюджета | `NoCapacity` с его именем |
| у одного из прочих узлов не хватает бюджета, остальных достаточно | он пропущен, размещение успешно |
| бюджета хватает меньше чем на `replicas` узлах | `NoCapacity` |
| заняты индексы 0 и 2 | выбран индекс 1 |
| занят `minor 1001` одним томом и `port 7000` другим | выбран индекс 2 |
| `maxPeers: 1` при 3 узлах | `MaxPeers { needed: 2 }` |
| равный бюджет | порядок выбора по имени |
| `parse_parameters`: пусто | `(3, C, 7)` |
| `parse_parameters`: `replicas: "2"`, `protocol: "A"`, `maxPeers: "5"` | `(2, A, 5)` |
| `parse_parameters`: `replicas: "two"`; `protocol: "B"`; ключ `foo` | ошибка с именем параметра |
| `parse_parameters`: `csi.storage.k8s.io/fstype: ext4` | игнорируется |
| `round_up_size`: `0`, `1`, `4096`, `4097`, `-1`, `i64::MAX` | `1 ГиБ`, `4096`, `4096`, `8192`, ошибка, ошибка |

Дополнительно свойство: для любого успешного результата `crd::to_volume_spec` проходит валидацию.

- [ ] **Step 2: реализовать.**

Run: `cargo test --locked placement`
Expected: тесты проходят.

- [ ] **Step 3: коммит.**

```bash
git add src/placement.rs src/tests/placement.rs
git commit -m "feat(placement): choose replica and witness nodes, allocate minor and port, check the budget" -- src
```

---

### Task 6: членство и коллизии (`membership`)

**Files:**
- Create: `src/membership.rs`, `src/tests/membership.rs`
- Modify: `src/lib.rs`, `src/model.rs` (длина имени узла)

**Interfaces:**
- Consumes: `crd::{ReplicatedVolume, ReplicatedVolumeSpec, MemberNode, NodeKind}`, `config::ClusterNodes`.
- Produces:

```rust
pub const FINALIZER_PREFIX: &str = "ambar.balookrd.github.io/replica-";
pub fn replica_finalizer(node: &str) -> String;
/// Finalizers a new object must carry: one per Replica node.
pub fn initial_finalizers(spec: &ReplicatedVolumeSpec) -> Vec<String>;

pub enum Join { AlreadyMember(NodeKind), Added }
/// Adds `node` as a Client. Errors: the node is not in the ConfigMap; no room within maxPeers.
pub fn join_as_client(spec: &mut ReplicatedVolumeSpec, node: &str, nodes: &ClusterNodes) -> Result<Join, MembershipError>;

pub enum Leave { NotAMember, Kept(NodeKind), Removed }
/// Removes `node` only when it is a Client.
pub fn leave_as_client(spec: &mut ReplicatedVolumeSpec, node: &str) -> Leave;

/// Names of other volumes that use the same minor or the same port.
pub fn collisions(name: &str, minor: u32, port: u16, all: &[(String, u32, u16)]) -> Vec<String>;

pub enum MembershipError { UnknownNode(String), NoRoom { max_peers: u32 }, NodeIdTaken { node: String, node_id: u32, by: String } }
```

`join_as_client`: `spec.nodes.len()` после добавления не больше `max_peers + 1`, иначе `NoRoom`; `nodeId` узла из `ClusterNodes` не занят другой записью тома, иначе `NodeIdTaken`; список остаётся отсортированным по имени.

Имя финализатора — qualified name Kubernetes: часть после `/` не длиннее 63 символов. `replica-` занимает 8, поэтому `model::valid_node_name` принимает имена длиной 1–55 символов (было 1–63) и требует, чтобы последний символ был буквой или цифрой. Правило в спеке меняет задача 15.

- [ ] **Step 1: тесты (падают).** `join_as_client`: новый узел → `Added`, запись `Client` с `nodeId` и адресом из `ClusterNodes`, порядок по имени; узел уже `Replica`/`Witness`/`Client` → `AlreadyMember(kind)`, список не изменён; узел не из `ClusterNodes` → `UnknownNode`; `max_peers = 2` и три записи → `NoRoom`; чужая запись с тем же `nodeId` → `NodeIdTaken`. `leave_as_client`: `Client` → `Removed`; `Replica` и `Witness` → `Kept`, список не изменён; нет записи → `NotAMember`. `collisions`: совпадение по `minor`, по `port`, по обоим, без совпадений, собственное имя исключается. `replica_finalizer("n1")` равен `ambar.balookrd.github.io/replica-n1`; `initial_finalizers` содержит финализаторы только узлов `Replica`. `valid_node_name`: 55 символов принимается, 56 отклоняется, имя с точкой или дефисом в конце отклоняется.
- [ ] **Step 2: реализовать.**

Run: `cargo test --locked`
Expected: все тесты проходят (включая тесты `model` с новой границей длины).

- [ ] **Step 3: коммит.**

```bash
git add src/membership.rs src/tests/membership.rs
git commit -m "feat(membership): client join and leave, per-replica finalizers, minor and port collisions" -- src
```

---

### Task 7: решение агента по объекту (`agent::decide`, `agent::status`)

**Files:**
- Create: `src/agent/mod.rs`, `src/agent/decide.rs`, `src/agent/status.rs`
- Create: `src/agent/tests/decide.rs`, `src/agent/tests/status.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `crd::*`, `membership::{replica_finalizer, collisions}`, `drbd::status::ResourceStatus`, `model::VolumeSpec`.
- Produces:

```rust
// src/agent/decide.rs
/// What the node holds for a volume name, read without running drbdadm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// The backing file exists.
    pub image: bool,
    /// A resource file, an applied copy, a device link or a configured kernel resource exists.
    pub traces: bool,
    /// The synced marker exists.
    pub synced: bool,
}

/// The object as the agent sees it; `None` when no object has this name.
pub struct ObjectView<'a> {
    pub spec: &'a ReplicatedVolumeSpec,
    pub deleting: bool,                // metadata.deletionTimestamp is set
    pub finalizers: &'a [String],
    pub collides_with: &'a [String],   // membership::collisions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// Nothing on this node concerns the volume.
    Ignore,
    /// Run Reconciler::apply with this description, then report status.
    Apply { spec: VolumeSpec, mark_done: bool },
    /// Delete the local copy (Reconciler::remove), then drop this node's finalizer.
    DeleteReplica,
    /// The local copy is already gone: drop this node's finalizer.
    DropFinalizer,
    /// Remove what a node without a disk leaves behind (Reconciler::remove on a node with no image).
    CleanDiskless,
    /// Do nothing and report: the state needs a human.
    Refuse(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A backing file exists for a volume that has no object.
    OrphanImage,
    /// A backing file exists but this node is not a Replica of the object.
    ImageWithoutMembership,
    /// The object is being deleted, this node is a Replica, but its finalizer is missing.
    DeletingWithoutFinalizer,
    Collision(Vec<String>),
    InvalidSpec(String),
}

pub fn decide(node: &str, volume: &str, object: Option<&ObjectView>, local: Local) -> Task;
```

Таблица решений `decide` (проверяется сверху вниз, первая подходящая строка):

| Объект | Узел в `spec.nodes` | Локально | Результат |
|---|---|---|---|
| нет | — | `image` | `Refuse(OrphanImage)` |
| нет | — | `traces`, нет `image` | `CleanDiskless` |
| нет | — | ничего | `Ignore` |
| удаляется | `Replica`, финализатор узла есть | `image` или `traces` | `DeleteReplica` |
| удаляется | `Replica`, финализатор узла есть | ничего | `DropFinalizer` |
| удаляется | `Replica`, финализатора узла нет | `image` | `Refuse(DeletingWithoutFinalizer)` |
| удаляется | `Replica`, финализатора узла нет | нет `image` | `CleanDiskless`, если `traces`, иначе `Ignore` |
| удаляется | `Witness`, `Client` или нет | `image` | `Refuse(ImageWithoutMembership)` |
| удаляется | `Witness`, `Client` или нет | нет `image` | `CleanDiskless`, если `traces`, иначе `Ignore` |
| есть | нет | `image` | `Refuse(ImageWithoutMembership)` |
| есть | нет | `traces`, нет `image` | `CleanDiskless` |
| есть | нет | ничего | `Ignore` |
| есть | `Witness` или `Client` | `image` | `Refuse(ImageWithoutMembership)` |
| есть | любой | `collides_with` не пуст | `Refuse(Collision)` |
| есть | любой | `to_volume_spec` вернул ошибку | `Refuse(InvalidSpec)` |
| есть | любой | — | `Apply { spec, mark_done }` |

`mark_done` истинно, когда `spec.initialSync == Skip`, узел — лидер начальной синхронизации (`VolumeSpec::initial_sync_leader`) и `local.synced`.

Инвариант R10, проверяемый перебором всех сочетаний входов: `DeleteReplica` возвращается только при `object.is_some() && deleting && kind == Replica && финализатор узла присутствует`. `CleanDiskless` никогда не возвращается при `local.image`.

```rust
// src/agent/status.rs
/// Entry of this node for status.nodes. `status == None`: the resource is not configured.
pub fn node_status(node: &str, status: Option<&ResourceStatus>, message: &str) -> NodeStatus;
/// A volume is usable for mounting on this node: the resource is configured,
/// has quorum, and the local disk or a connected peer's disk is UpToDate.
pub fn usable(status: Option<&ResourceStatus>) -> bool;
/// Every Replica node has reported disk UpToDate (CreateVolume waits for this).
pub fn all_replicas_up_to_date(spec: &ReplicatedVolumeSpec, status: &ReplicatedVolumeStatus) -> bool;
```

- [ ] **Step 1: тесты `decide` (падают).** По тесту на строку таблицы; переборный тест инварианта R10 (объект ∈ {нет, есть, удаляется} × членство ∈ {нет, `Replica`, `Witness`, `Client`} × финализатор ∈ {есть, нет} × `image` × `traces` × `synced` × коллизия × `initialSync`); тесты `mark_done`: не лидер → ложь; `Done` → ложь; нет маркера → ложь.
- [ ] **Step 2: тесты `status` (падают).** На образцах `stand/spike/samples/*.json` (они уже используются в `src/drbd/tests/status.rs`): `node_status` для узла с копией, для узла без диска, для ненастроенного ресурса (`role: "Unknown"`, `disk: "Unknown"`, `quorum: false`, пустые соединения); `usable`: локальный `UpToDate` с кворумом → истина; локальный `Inconsistent`, сторона `UpToDate` и `Connected` → истина; нет кворума → ложь; узел без диска и все стороны не `UpToDate` → ложь; `None` → ложь. `all_replicas_up_to_date`: записи нет у одного `Replica` → ложь; `Witness` и `Client` не учитываются.
- [ ] **Step 3: реализовать.**

Run: `cargo test --locked agent`
Expected: тесты проходят.

- [ ] **Step 4: проверить каждое условие.** Для каждой строки таблицы с `Refuse` и для условий `DeleteReplica` временно убрать условие, убедиться, что падает тест, вернуть. Соответствие «условие → тест» записать в отчёт.

- [ ] **Step 5: коммит.**

```bash
git add src/agent
git commit -m "feat(agent): decide what a node does for a ReplicatedVolume; build the node status entry" -- src
```

---

### Task 8: цикл согласования агента

**Files:**
- Create: `src/agent/kube.rs` (вызовы API), `src/agent/run.rs` (цикл), `src/agent/local.rs` (чтение `Local`)
- Create: `src/agent/tests/local.rs`, `src/agent/tests/run.rs`
- Modify: `src/main.rs` (команда `agent`), `src/reconcile.rs` (доступ к перечню локальных томов)

**Interfaces:**
- Consumes: `agent::decide::*`, `agent::status::*`, `reconcile::Reconciler`, `paths::Paths`, `exec::Runner`, `membership::*`, `crd::*`.
- Produces:

```rust
// src/agent/local.rs
/// Volume names this node has anything for: backing files, resource files,
/// applied copies, device links and configured kernel resources.
pub fn local_volumes(paths: &Paths, runner: &dyn Runner) -> Result<BTreeSet<String>, ReconcileError>;
pub fn local(paths: &Paths, configured: &BTreeSet<String>, volume: &str) -> Result<Local, ReconcileError>;

// src/agent/kube.rs — the only module that calls the API server besides src/csi
#[async_trait]   // or native async trait, by the pinned toolchain
pub trait Volumes: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError>;
    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError>;
    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError>;      // Created::{New, Exists}
    async fn delete(&self, name: &str) -> Result<(), ApiError>;                          // not found is Ok
    /// get -> edit -> replace with the read resourceVersion; retried on conflict up to 5 times.
    /// `edit` returns false when no change is needed.
    async fn edit(&self, name: &str, edit: &(dyn Fn(&mut ReplicatedVolume) -> bool + Send + Sync)) -> Result<Option<ReplicatedVolume>, ApiError>;
    /// Server-side apply of this node's entry in status.nodes; `None` removes the entry.
    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError>;
}
pub struct KubeVolumes { /* kube::Api<ReplicatedVolume> */ }

// src/agent/run.rs
pub struct Agent {
    pub node: String,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    pub volumes: Arc<dyn Volumes>,
    /// Serialises work on one node: the reconcile loop and CSI Node calls take it per volume.
    pub locks: VolumeLocks,
    /// Set after the first full pass; CSI Node answers UNAVAILABLE before that.
    pub ready: Arc<AtomicBool>,
}
impl Agent {
    /// One pass for one volume name: read the object and the local state, decide, act, report status.
    pub async fn reconcile_one(&self, volume: &str) -> Result<Outcome, AgentError>;
    /// Watches objects, runs reconcile_one for changed ones and for all on a timer.
    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), AgentError>;
}
pub enum Outcome { Settled, Progress, Refused(Refusal) }
pub struct VolumeLocks { /* per-name tokio Mutex */ }
```

Поведение `reconcile_one`:

1. Взять блокировку имени в `VolumeLocks`.
2. Прочитать объект (`volumes.get`), все объекты для коллизий (из кэша наблюдения; в тестах — `volumes.list`), `local`.
3. `decide`, затем:

| `Task` | Действия |
|---|---|
| `Ignore` | ничего; `Settled` |
| `Apply` | `Reconciler::apply` в `spawn_blocking`. `ReconcileError::Busy` → `Progress` без записи статуса. Затем прочитать `Drbd::status`, записать `apply_node_status` (при ошибке `apply` — с её текстом в `message`). Если `mark_done` — `volumes.edit`: `initialSync` из `Skip` в `Done`. `Progress`, если `apply` вернул непустой список действий или запись статуса изменилась, иначе `Settled` |
| `DeleteReplica` | Прочитать `Drbd::status`; если локальная роль `Primary` или у стороны `peer_role == Primary` — ничего не удалять, статус с причиной, `Progress` (R18). Иначе `Reconciler::remove` в `spawn_blocking`; при успехе — `apply_node_status(…, None)`, затем `volumes.edit`: убрать финализатор узла. При ошибке (`down` отклонён, пока том смонтирован) — статус с текстом ошибки, `Progress` |
| `DropFinalizer` | `apply_node_status(…, None)`, `volumes.edit`: убрать финализатор узла |
| `CleanDiskless` | перечитать `local`; если `image` появился — ничего не делать; иначе `Reconciler::remove`; `apply_node_status(…, None)`, если объект есть |
| `Refuse` | сообщение в журнал уровня error с именем тома и причиной; если объект есть — статус с текстом причины в `message`; локальное состояние не меняется |

Запись статуса пропускается, если запись равна последней записанной этим процессом для тома.

Поведение `run`:

1. При старте: `module_loader::load` (если модуль не загрузился — завершение с ошибкой), проверка `NODE_NAME` против `/proc/sys/kernel/hostname`, проверка, что `paths.volumes_dir` существует.
2. Наблюдение за `ReplicatedVolume` (`kube::runtime::watcher` с хранилищем-отражателем). Событие об объекте ставит его имя в очередь.
3. Очередь имён с временем следующего запуска: `Progress` → через 1 с; `Settled` → через 30 с; `Refused` → через 60 с; ошибка API → через 5 с.
4. Раз в 30 с в очередь добавляются все имена из `local_volumes` и из хранилища (так находятся остатки без объекта).
5. После первого полного прохода по объединению имён — `ready = true`.
6. Имена обрабатываются последовательно в одной задаче.

- [ ] **Step 1: тесты `local` (падают).** `tempfile`-каталоги и поддельный `Runner` с выводом `drbdsetup status --json`: том с файлом `.img`; только `.res`; только `.res.applied`; только ссылка в `dev_dir`; только настроенный ресурс в ядре; маркер `.img.synced`; файл `.lock` и маркеры без `.img` не считаются `traces`, и том, у которого есть только они, не попадает в `local_volumes`. Имена, не проходящие `valid_volume_name`, пропускаются.

- [ ] **Step 2: тесты `reconcile_one` (падают).** Поддельные `Volumes` (объекты в `Mutex<BTreeMap>`, журнал вызовов) и поддельный `Runner` из тестов `reconcile`. Случаи:

| Случай | Проверка |
|---|---|
| узел `Replica`, новый том | вызван `apply`, записан статус, `Progress` |
| повтор без изменений | статус не записан второй раз, `Settled` |
| лидер, `Skip`, маркер `.img.synced` есть | объект получает `initialSync: Done` |
| не лидер, `Skip`, маркер есть | объект не меняется |
| удаление, узел `Replica` с финализатором | `remove` вызван, запись статуса удалена, финализатор узла снят, финализаторы других узлов на месте |
| удаление, сторона в роли `Primary` | `remove` не вызван, финализатор и файл тома на месте, в статусе причина |
| удаление, локальная роль `Primary` | то же |
| удаление, `drbdadm down` возвращает ошибку | финализатор на месте, файл тома на месте, в статусе текст ошибки |
| объекта нет, есть `.img` | ни одной команды `drbdadm`, `losetup` и ни одного удаления файла; `Refused(OrphanImage)` |
| узел убран из `spec.nodes`, остался `.res` без `.img` | `remove` вызван |
| коллизия `minor` | `apply` не вызван, в статусе текст с именем второго тома |
| `apply` вернул `Busy` | статус не записан, `Progress` |
| `volumes.edit` получает конфликт дважды | третья попытка успешна |

- [ ] **Step 3: реализовать `local`, `Volumes`/`KubeVolumes`, `Agent`.** Команда `ambar agent` с параметрами: `--node-name` (env `NODE_NAME`, обязателен), `--csi-socket` (по умолчанию `/var/lib/kubelet/plugins/ambar/csi.sock`), `--namespace` (env `POD_NAMESPACE`, по умолчанию `ambar-system`), `--modules-dir`. В этой задаче `agent` запускает только цикл; сервер CSI подключает задача 10. Завершение по `SIGTERM`: остановить приём событий, дождаться текущего `reconcile_one`.

Run: `cargo test --locked`
Expected: все тесты проходят.

- [ ] **Step 4: проверка на стенде без CSI.** Собрать образ, установить CRD, запустить агент вручную в поде `toolbox` на трёх узлах (`ambar agent` в фоне, kubeconfig из сервисного аккаунта пода; права выдаются временным `ClusterRoleBinding` на `cluster-admin` для сервисного аккаунта `ambar-spike/default`, который удаляется в конце шага). Создать объект `rv-smoke` (три `Replica`, `initialSync: Skip`, финализаторы трёх узлов, адреса и `nodeId` стенда) командой `kc apply`.

Expected: через ≤ 30 с `kc get rv rv-smoke -o json` показывает три записи `status.nodes` с `disk: UpToDate` и `spec.initialSync: Done`. После `kc delete rv rv-smoke` объект исчезает за ≤ 30 с, на узлах нет `rv-smoke.img`, `drbdsetup status` не показывает ресурс. Вывод — в отчёт.

- [ ] **Step 5: коммит.**

```bash
git add src/agent
git commit -m "feat(agent): reconcile loop over ReplicatedVolume objects with per-node status and finalizers" -- src Cargo.toml Cargo.lock
```

---

### Task 9: монтирование (`mount`)

**Files:**
- Create: `src/mount.rs`, `src/tests/mount.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `exec::Runner`; записи P9 из задачи 1.
- Produces:

```rust
pub struct MountEntry { pub source: String, pub target: PathBuf, pub fstype: String, pub read_only: bool }
/// Parses /proc/self/mountinfo (octal escapes like \040 in paths are decoded).
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry>;
pub fn mounted_at<'a>(entries: &'a [MountEntry], target: &Path) -> Option<&'a MountEntry>;

#[derive(Debug, PartialEq, Eq)]
pub enum Signature { Ext4, Empty, Other(String) }

pub struct Mounter<'a> { pub runner: &'a dyn Runner, pub mountinfo: &'a Path }
impl Mounter<'_> {
    /// Opens the device read-only, reads the ext4 magic and asks blkid.
    pub fn signature(&self, device: &Path) -> Result<Signature, MountError>;
    pub fn mkfs_ext4(&self, device: &Path) -> Result<(), MountError>;
    /// Idempotent: Ok when `target` already has `device` mounted; error when something else is mounted there.
    pub fn mount_device(&self, device: &Path, target: &Path) -> Result<(), MountError>;
    pub fn bind(&self, source: &Path, target: &Path, read_only: bool) -> Result<(), MountError>;
    /// Idempotent: Ok when nothing is mounted at `target`.
    pub fn unmount(&self, target: &Path) -> Result<(), MountError>;
}
pub enum MountError {
    /// open(2) failed; `in_use` is true for EMEDIUMTYPE.
    Open { device: String, in_use: bool, source: io::Error },
    Command { command: String, code: Option<i32>, stderr: String },
    /// The two signature checks disagree, or blkid failed in a way that is not "nothing found".
    AmbiguousSignature { device: String, detail: String },
    WrongFilesystem { device: String, found: String },
    Busy { target: String, source_mounted: String },
    Io { action: &'static str, path: String, source: io::Error },
}
```

Правило `signature` (ни одно условие не заменяется другим):

| Чтение магии ext4 (2 байта по смещению 1080) | `blkid -p -o value -s TYPE` | Результат |
|---|---|---|
| ошибка открытия или чтения | не запускается | `Open` |
| `53 ef` | код 0, `ext4` | `Ext4` |
| не `53 ef` | код и вывод из P9 для устройства без файловой системы, stderr пуст | `Empty` |
| не `53 ef` | код 0, другой тип | `Other(тип)` |
| любое другое сочетание | | `AmbiguousSignature` |

`mkfs.ext4 -q <устройство>` вызывается вызывающим кодом (задача 12) только при `Signature::Empty`. Код возврата и вывод `blkid` для пустого устройства берутся из протокола P9 дословно и хранятся константами с комментарием-ссылкой.

`mount_device`: `mount -t ext4 <устройство> <каталог>`. `bind`: `mount --bind`, при `read_only` затем `mount -o remount,bind,ro`. `unmount`: `umount <каталог>`. Сравнение «то же устройство» — по `source` из `mountinfo` (`/dev/drbd<minor>`).

- [ ] **Step 1: тесты (падают).** `parse_mountinfo` на трёх строках, снятых со стенда (корень, bind-монтирование, путь с пробелом как `\040`), плюс пустой ввод и строка без разделителя ` - ` (пропускается). `signature`: файл-заглушка вместо устройства (во временном каталоге) с магией и без, поддельный `Runner` с ответами `blkid` для каждой строки таблицы; отсутствующий файл → `Open { in_use: false }`. `mount_device`: уже смонтировано то же устройство → ни одной команды; смонтировано другое → `Busy`; не смонтировано → одна команда `mount` с точными аргументами. `unmount`: не смонтировано → ни одной команды. `bind` с `read_only` → две команды в заданном порядке. Тест с `EMEDIUMTYPE` невозможен без DRBD: соответствие `raw_os_error() == Some(libc::EMEDIUMTYPE)` → `in_use: true` вынесено в функцию `open_error(&io::Error) -> bool` и проверяется на `io::Error::from_raw_os_error(124)` под `#[cfg(target_os = "linux")]`.

- [ ] **Step 2: реализовать.** Константа `EMEDIUMTYPE` — `124` под `cfg(target_os = "linux")`, без зависимости `libc`; на других ОС функция возвращает `false`.

Run: `cargo test --locked mount && docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked mount`
Expected: тесты проходят на macOS и в контейнере Linux.

- [ ] **Step 3: коммит.**

```bash
git add src/mount.rs src/tests/mount.rs
git commit -m "feat(mount): mountinfo parsing, two-source filesystem signature check, idempotent mount and unmount" -- src
```

---

### Task 10: типы CSI, сервер и Identity

**Files:**
- Create: `proto/csi.proto` (из репозитория `container-storage-interface/spec`, последний выпуск v1; номер выпуска и адрес файла — в комментарии первой строкой и в отчёте), `build.rs`
- Create: `src/csi/mod.rs`, `src/csi/identity.rs`, `src/csi/tests/identity.rs`
- Modify: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `.github/workflows/ci.yml` (если сборке нужен `protoc`)

**Interfaces:**
- Produces:

```rust
// src/csi/mod.rs
pub mod proto { /* generated: csi.v1 */ }
/// Removes a stale socket file, binds the unix socket with mode 0600 and serves until `shutdown`.
pub async fn serve(socket: &Path, services: Services, shutdown: CancellationToken) -> Result<(), CsiError>;
pub struct Services { pub identity: IdentityService, pub controller: ControllerService, pub node: NodeService }

// src/csi/identity.rs
pub struct IdentityService { pub ready: Arc<AtomicBool> }
```

Ответы Identity:

| Вызов | Ответ |
|---|---|
| `GetPluginInfo` | `name = "ambar.balookrd.github.io"`, `vendor_version = env!("CARGO_PKG_VERSION")` |
| `GetPluginCapabilities` | `CONTROLLER_SERVICE`, `VOLUME_ACCESSIBILITY_CONSTRAINTS` |
| `Probe` | `ready` = значение флага агента |

Генерация кода: `build.rs` компилирует `proto/csi.proto`. Компилятор `protoc` не должен требовать ручной установки на машине разработчика и в CI: исполнитель выбирает способ, поддерживаемый зафиксированной версией `tonic` (вендоренный `protoc` через крейт либо чистый Rust-парсер), проверяет, что `cargo zigbuild --target aarch64-unknown-linux-musl` и задачи CI проходят, и записывает выбор. Файл `proto/csi.proto` распространяется под Apache-2.0, как и репозиторий.

`ControllerService` и `NodeService` в этой задаче — структуры, у которых каждый вызов возвращает `UNIMPLEMENTED`; их наполняют задачи 11 и 12.

- [ ] **Step 1: тесты Identity (падают).** Вызовы методов сервиса напрямую (без сокета): три ответа из таблицы; `Probe` при `ready = false` возвращает `ready: false`.
- [ ] **Step 2: тест сервера (падает).** Запустить `serve` на сокете во временном каталоге, подключиться клиентом `tonic` через unix-сокет, вызвать `GetPluginInfo`; после отмены `shutdown` функция возвращается, повторный запуск на том же пути проходит (старый файл сокета удаляется).
- [ ] **Step 3: реализовать; подключить `serve` к команде `agent`** (цикл согласования и сервер работают в одном рантайме, общий `CancellationToken`).

Run: `cargo test --locked csi && cargo zigbuild --release --locked --target aarch64-unknown-linux-musl`
Expected: тесты проходят, бинарь собран.

- [ ] **Step 4: коммит.**

```bash
git add proto build.rs src/csi
git commit -m "feat(csi): CSI v1 types, unix socket server and the Identity service" -- Cargo.toml Cargo.lock build.rs proto src .github/workflows/ci.yml
```

---

### Task 11: CSI Controller

**Files:**
- Modify: `src/csi/controller.rs`
- Create: `src/csi/tests/controller.rs`
- Create: `src/agent/nodes.rs` (чтение `ConfigMap ambar-nodes`), `src/agent/tests/nodes.rs`

**Interfaces:**
- Consumes: `placement::*`, `membership::initial_finalizers`, `agent::kube::Volumes`, `agent::status::all_replicas_up_to_date`, `config::ClusterNodes`.
- Produces:

```rust
// src/agent/nodes.rs
#[async_trait]
pub trait NodeMap: Send + Sync { async fn load(&self) -> Result<ClusterNodes, ApiError>; }
pub struct ConfigMapNodes { /* kube::Api<ConfigMap> in the agent namespace */ }

// src/csi/controller.rs
pub struct ControllerService {
    pub volumes: Arc<dyn Volumes>,
    pub nodes: Arc<dyn NodeMap>,
    pub create_wait: Duration,   // 60 s
    pub delete_wait: Duration,   // 60 s
    pub poll: Duration,          // 500 ms
}
pub const TOPOLOGY_KEY: &str = "ambar.balookrd.github.io/node";
```

`CreateVolume`:

| Шаг | Условие | Код gRPC |
|---|---|---|
| 1 | `name` пусто или не проходит `valid_volume_name` | `INVALID_ARGUMENT` |
| 2 | `volume_capabilities` пуст; есть capability не `mount`; `fs_type` не пуст и не `ext4`; режим доступа не из {`SINGLE_NODE_WRITER`, `SINGLE_NODE_SINGLE_WRITER`, `SINGLE_NODE_MULTI_WRITER`, `SINGLE_NODE_READER_ONLY`} | `INVALID_ARGUMENT` |
| 3 | `volume_content_source` задан | `INVALID_ARGUMENT` |
| 4 | `parse_parameters`, `round_up_size` вернули ошибку; `limit_bytes` задан и меньше размера | `INVALID_ARGUMENT`, `OUT_OF_RANGE` для размера |
| 5 | объект с этим именем есть, `sizeBytes`, `protocol`, `maxPeers` и число `Replica` совпадают с запросом | перейти к шагу 8 |
| 6 | объект есть, параметры отличаются | `ALREADY_EXISTS` |
| 7 | `place` по всем существующим объектам: `Replicas`, `NotEnoughNodes`, `UnknownSelectedNode`, `MaxPeers` → `INVALID_ARGUMENT`; `NoCapacity`, `NoFreeIndex` → `RESOURCE_EXHAUSTED`. Создать объект с `initial_finalizers`. `Created::Exists` → вернуться к шагу 5 | |
| 8 | опрашивать объект каждые `poll`, пока `all_replicas_up_to_date`; по истечении `create_wait` | `DEADLINE_EXCEEDED` |
| 9 | ответ: `volume_id = name`, `capacity_bytes = sizeBytes`, без `accessible_topology` | `OK` |

`selected_node` — значение сегмента `TOPOLOGY_KEY` из `accessibility_requirements.preferred[0]`; если `preferred` пуст — из `requisite[0]`; если требований нет — `None`.

Код ошибки при `replicas < 2` и при нехватке узлов: `INVALID_ARGUMENT` (закрывает `[проверить: код ошибки]` в спеке).

`DeleteVolume`: пустой `volume_id` → `INVALID_ARGUMENT`; имя не проходит `valid_volume_name` или объекта нет → `OK`; иначе `volumes.delete`, затем опрос каждые `poll`, пока объект не исчезнет; по истечении `delete_wait` → `DEADLINE_EXCEEDED` (provisioner повторит вызов).

`ControllerGetCapabilities`: `CREATE_DELETE_VOLUME`. `ValidateVolumeCapabilities`: объекта нет → `NOT_FOUND`; иначе `confirmed` с переданными capability, если они проходят проверку шага 2, иначе пустой `confirmed` с `message`. Остальные вызовы — `UNIMPLEMENTED`.

- [ ] **Step 1: тесты (падают).** Поддельные `Volumes` и `NodeMap`, `tokio::time::pause` для ожиданий. По тесту на каждую строку таблицы `CreateVolume`; дополнительно: успешное создание — объект имеет `initialSync: Skip` и финализаторы узлов `Replica`; `selected_node` берётся из `preferred[0]`; второй вызов с тем же именем, пока первый объект не готов, не создаёт второй объект; два тома подряд получают индексы 0 и 1; `DeleteVolume` для отсутствующего объекта — `OK`; `DeleteVolume`, когда объект остаётся с финализатором, — `DEADLINE_EXCEEDED` и ровно один вызов `delete`. `ConfigMapNodes`: отсутствие карты или ключа `nodes` → ошибка с именем карты.
- [ ] **Step 2: реализовать.**

Run: `cargo test --locked csi agent`
Expected: тесты проходят.

- [ ] **Step 3: коммит.**

```bash
git add src/csi src/agent
git commit -m "feat(csi): Controller service — CreateVolume and DeleteVolume over ReplicatedVolume objects" -- src
```

---

### Task 12: CSI Node

**Files:**
- Modify: `src/csi/node.rs`
- Create: `src/csi/tests/node.rs`

**Interfaces:**
- Consumes: `agent::run::Agent` (`reconcile_one`, `locks`, `ready`), `agent::kube::Volumes`, `agent::nodes::NodeMap`, `membership::{join_as_client, leave_as_client}`, `mount::Mounter`, `agent::status::usable`, `drbd::adm::Drbd`; записи P8 и P9.
- Produces:

```rust
pub struct NodeService {
    pub agent: Arc<Agent>,
    pub nodes: Arc<dyn NodeMap>,
    pub mountinfo: PathBuf,        // /proc/self/mountinfo
    pub stage_wait: Duration,      // 30 s
    pub poll: Duration,            // 500 ms
}
```

`NodeGetInfo`: `node_id` = имя узла; `accessible_topology` = `{TOPOLOGY_KEY: имя узла}`; `max_volumes_per_node` = 0. `NodeGetCapabilities`: `STAGE_UNSTAGE_VOLUME`.

`NodeStageVolume(volume_id, staging_target_path, volume_capability)`:

| Шаг | Действие | Ошибка |
|---|---|---|
| 1 | Агент не готов (`ready == false`) | `UNAVAILABLE` |
| 2 | Пустые поля; имя не проходит `valid_volume_name`; capability не `mount` или `fs_type` не пуст и не `ext4` | `INVALID_ARGUMENT` |
| 3 | Объекта нет либо у него `deletionTimestamp` | `NOT_FOUND` |
| 4 | Каталог staging уже содержит монтирование `/dev/drbd<minor>` | `OK` |
| 5 | `volumes.edit` с `join_as_client`. `UnknownNode` → `FAILED_PRECONDITION` («узла нет в ambar-nodes»); `NoRoom`, `NodeIdTaken` → `RESOURCE_EXHAUSTED` | |
| 6 | `agent.reconcile_one(volume)`; `Refused` → `FAILED_PRECONDITION` с причиной | |
| 7 | Опрашивать `Drbd::status` каждые `poll`, пока `usable`; между опросами повторять `reconcile_one`, если прошлый вернул `Progress`. По истечении `stage_wait` | `UNAVAILABLE` |
| 8 | `Mounter::signature`: `Open { in_use: true }` → `FAILED_PRECONDITION` («том открыт на другом узле»); другая `Open`, `AmbiguousSignature` → `UNAVAILABLE`; `Other` → `FAILED_PRECONDITION`; `Empty` → `mkfs_ext4`; `Ext4` → дальше | |
| 9 | Создать каталог staging, `mount_device`. Ошибка `mount` → `FAILED_PRECONDITION` с stderr | |

Шаги 4–9 выполняются под блокировкой имени из `agent.locks` только на время шагов 8–9; `reconcile_one` берёт её сам.

`NodePublishVolume`: проверки полей; в каталоге staging нет монтирования → `FAILED_PRECONDITION`; создать `target_path`; `bind` с `read_only` из запроса; повторный вызов → `OK`.

`NodeUnpublishVolume`: `unmount(target_path)`, удалить пустой каталог; нет монтирования → `OK`.

`NodeUnstageVolume`:

| Шаг | Действие |
|---|---|
| 1 | `unmount(staging_target_path)`; ошибка `umount` → `FAILED_PRECONDITION` с stderr, дальше не идти |
| 2 | Объекта нет → `OK` |
| 3 | `volumes.edit` с `leave_as_client`. `Kept` и `NotAMember` → `OK` без локальных действий |
| 4 | `Removed` → `agent.reconcile_one(volume)`: узел уже не участник, файла тома нет, поэтому `decide` даёт `CleanDiskless`. Ошибка уборки не возвращается kubelet (остатки уберёт цикл), пишется в журнал |

Порядок шагов 3–4 (сначала правка объекта, затем локальная уборка) соответствует R11; порядок команд DRBD при уходе стороны сверяется с записью P8. Если P8 показал, что сторону нужно сначала опустить локально, а затем убирать из файла ресурса остальных узлов, порядок шагов 3 и 4 меняется на обратный, и это записывается в отчёт и в спеку.

Остальные вызовы Node — `UNIMPLEMENTED`.

- [ ] **Step 1: тесты (падают).** Поддельные `Volumes`, `NodeMap`, `Runner`; каталоги во `tempfile`; файл `mountinfo` подменяется. По тесту на каждую строку таблиц; дополнительно:
  - узел `Replica`: `NodeStageVolume` не меняет `spec.nodes`;
  - узел не участник: после `NodeStageVolume` в объекте запись `Client` этого узла;
  - `signature == Ext4`: команда `mkfs.ext4` не вызывается;
  - `signature == Empty`: `mkfs.ext4` вызывается один раз, затем `mount`;
  - `AmbiguousSignature`, `Open`: ни `mkfs.ext4`, ни `mount` не вызываются;
  - `NodeUnstageVolume` на узле `Witness`: запись остаётся, `drbdadm down` не вызывается;
  - `NodeUnstageVolume` на узле `Client`: записи нет, вызваны `drbdadm down` и удаление файла ресурса;
  - `NodeUnstageVolume`, когда `umount` вернул ошибку: объект не меняется;
  - повторные `NodeStageVolume`, `NodePublishVolume`, `NodeUnpublishVolume`, `NodeUnstageVolume` возвращают `OK` без команд монтирования.
- [ ] **Step 2: реализовать.**

Run: `cargo test --locked && docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`
Expected: все тесты проходят на macOS и в контейнере Linux.

- [ ] **Step 3: проверить условия `mkfs`.** Временно ослабить каждое из условий шага 8 (считать `AmbiguousSignature` пустым устройством; считать ошибку открытия пустым устройством), убедиться, что падает тест, вернуть. Соответствие — в отчёт.

- [ ] **Step 4: коммит.**

```bash
git add src/csi
git commit -m "feat(csi): Node service — stage with on-demand client membership, publish, unpublish, unstage" -- src
```

---

### Task 13: манифесты, образ, установка на стенд

**Files:**
- Create: `deploy/namespace.yaml`, `deploy/csidriver.yaml`, `deploy/rbac.yaml`, `deploy/daemonset.yaml`, `deploy/storageclass.yaml`, `deploy/nodes-configmap.example.yaml`
- Create: `stand/install.sh`
- Modify: `image/Dockerfile`, `image/test.sh` (если нужны пакеты для `mkfs.ext4`, `blkid`, `mount`)
- Modify: `.github/workflows/ci.yml` (проверка синтаксиса манифестов)

**Interfaces:**
- Consumes: `deploy/crd.json`, команда `ambar agent`.
- Produces: `stand/install.sh` — устанавливает драйвер на стенд; `stand/install.sh --remove` — удаляет.

`deploy/csidriver.yaml` — поля из раздела спеки «`CSIDriver`» без изменений.

`deploy/storageclass.yaml` — два класса: `ambar-r3` (`replicas: "3"`) и `ambar-r2` (`replicas: "2"`); у обоих `provisioner: ambar.balookrd.github.io`, `volumeBindingMode: WaitForFirstConsumer`, `allowVolumeExpansion: false`, `reclaimPolicy: Delete`, `parameters` с `protocol: "C"` и `csi.storage.k8s.io/fstype: ext4`.

`deploy/daemonset.yaml`, DaemonSet `ambar` в `ambar-system`:

| Поле | Значение |
|---|---|
| `hostNetwork` | `true` |
| `priorityClassName` | `system-node-critical` |
| `tolerations` | `operator: Exists` |
| `updateStrategy` | `RollingUpdate`, `maxUnavailable: 1` |
| контейнер `ambar` | образ `ambar-drbd:dev` (в манифесте имя образа — параметр установки), команда `ambar agent`, `privileged: true`, env `NODE_NAME` (`spec.nodeName`), `POD_NAMESPACE` (`metadata.namespace`) |
| тома контейнера `ambar` | `/dev` ← `hostPath /dev`; `/lib/modules` ← `hostPath`, только чтение; `/etc/drbd.d` ← `hostPath /var/lib/rancher/ambar/res`, `type: Directory`; `/var/lib/rancher/ambar/volumes` ← `hostPath`, `type: Directory`; `/var/lib/kubelet` ← `hostPath`, `mountPropagation: Bidirectional`; каталог сокета `/var/lib/kubelet/plugins/ambar` ← `hostPath`, `type: DirectoryOrCreate` |
| контейнер `node-driver-registrar` | аргументы `--csi-address=/csi/csi.sock`, `--kubelet-registration-path=/var/lib/kubelet/plugins/ambar/csi.sock`; тома: каталог сокета как `/csi`, `/var/lib/kubelet/plugins_registry` как `/registration` |
| контейнер `csi-provisioner` | аргументы `--csi-address=/csi/csi.sock`, `--leader-election`, `--leader-election-namespace=$(POD_NAMESPACE)`, `--feature-gates=Topology=true`, `--strict-topology`, `--timeout=90s`, `--default-fstype=ext4`; том: каталог сокета как `/csi` |

Образы сайдкаров — `registry.k8s.io/sig-storage/csi-node-driver-registrar` и `registry.k8s.io/sig-storage/csi-provisioner`, последние выпуски, закреплённые по digest. Набор аргументов сверяется с документацией закреплённых выпусков: если аргумент переименован или включён по умолчанию, манифест следует документации, расхождение с таблицей записывается в отчёт.

`deploy/rbac.yaml`: `ServiceAccount ambar`; `ClusterRole` агента:

| Ресурс | Права |
|---|---|
| `replicatedvolumes.ambar.balookrd.github.io` | `get`, `list`, `watch`, `create`, `update`, `patch`, `delete` |
| `replicatedvolumes/status` | `get`, `patch` |

`Role` в `ambar-system`: `configmaps` — `get` для `resourceNames: [ambar-nodes]`. Права `external-provisioner` — из файла `deploy/kubernetes/rbac.yaml` закреплённого выпуска `external-provisioner` (кластерная роль и роль для выбора лидера), привязанные к `ServiceAccount ambar`; источник указывается комментарием в манифесте. Права на `pods` и `nodes` из раздела спеки «Права агента (RBAC)» в этой фазе не выдаются: они нужны фазе 3.

`stand/install.sh`:

1. Создаёт на каждом узле каталоги `/var/lib/rancher/ambar/volumes` (0700) и `/var/lib/rancher/ambar/res`.
2. Удаляет DaemonSet `toolbox` из `ambar-spike` (два процесса не должны управлять одними ресурсами DRBD) и ждёт завершения его подов.
3. Импортирует образ агента на узлы (как `stand/k3s.sh`).
4. Применяет `deploy/namespace.yaml`, `deploy/crd.json`, `deploy/csidriver.yaml`, `deploy/rbac.yaml`.
5. Генерирует `ConfigMap ambar-nodes` из `NODES`: имя `vm_host`, адрес `vm_ip`, `nodeId` по порядку в `NODES`, `capacityBytes` 8 ГиБ.
6. Применяет `deploy/daemonset.yaml` и `deploy/storageclass.yaml`, ждёт `kc -n ambar-system rollout status ds/ambar --timeout=180s`.
7. Проверяет: `kc get csinode <узел> -o jsonpath='{.spec.drivers[*].name}'` содержит `ambar.balookrd.github.io` для каждого узла.

`--remove`: удаляет классы, DaemonSet, RBAC, `CSIDriver`, CRD (только если объектов `ReplicatedVolume` нет; иначе завершается с ошибкой и списком объектов), пространство имён; возвращает DaemonSet `toolbox` (`kc apply -f stand/toolbox.yaml`).

- [ ] **Step 1: проверить состав образа.**

Run: `docker run --rm ambar-drbd:dev sh -c 'command -v mkfs.ext4 blkid mount umount'`
Expected: четыре пути. Если какого-то нет — добавить пакет в `image/Dockerfile` и проверку в `image/test.sh`.

- [ ] **Step 2: написать манифесты и `stand/install.sh`.**

- [ ] **Step 3: установить на стенд.**

Run: `image/build.sh && stand/install.sh`
Expected: последняя строка `OK: ambar installed on <N> nodes`; `kc -n ambar-system get pods` показывает по одному поду `ambar` на узел, все контейнеры `Running`; в журнале контейнера `ambar` строка о загруженном модуле `drbd 9.3.4` и нет строк уровня error.

- [ ] **Step 4: проверить удаление.**

Run: `stand/install.sh --remove && stand/check.sh && stand/install.sh`
Expected: оба запуска установки и `stand/check.sh` завершаются с кодом 0.

- [ ] **Step 5: CI.** В задачу `image-scripts` добавить проверку, что каждый файл `deploy/*.yaml` разбирается как YAML (`python3 -c 'import sys,yaml; [list(yaml.safe_load_all(open(f))) for f in sys.argv[1:]]' deploy/*.yaml`), а `deploy/crd.json` — как JSON; `bash -n` для новых скриптов уже покрыт маской `stand/*.sh`.

- [ ] **Step 6: коммит.**

```bash
git add deploy stand/install.sh
git commit -m "feat(deploy): manifests of the driver and the stand install script" -- deploy stand/install.sh image .github/workflows/ci.yml
```

---

### Task 14: четвёртая ВМ и сквозная проверка на стенде

**Files:**
- Modify: `stand/lib.sh` (переменная окружения `NODES`), `stand/up.sh`, `stand/k3s.sh`, `stand/check.sh`, `stand/down.sh` (если в них зашито число узлов)
- Create: `stand/csi-e2e.sh`
- Modify: `stand/README.md`, `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` (раздел «Проверки CSI на стенде»)

**Interfaces:**
- Consumes: `stand/install.sh`, классы `ambar-r3` и `ambar-r2`.
- Produces: `stand/csi-e2e.sh` — последняя строка `OK: csi e2e`; код возврата 0 только при успехе всех шагов.

`stand/lib.sh`: `NODES` берётся из окружения (`NODES="dv-1 dv-2 dv-3 dv-4"`), по умолчанию три узла, как сейчас. Четвёртая ВМ создаётся тем же шаблоном `stand/lima/node.yaml` и входит в кластер k3s как агент (не сервер), чтобы etcd оставался из трёх участников.

Шаги `stand/csi-e2e.sh` (каждый печатает заголовок и проверяет результат; при ошибке скрипт печатает `kc get rv -o yaml`, события пространства имён `default` и хвост журнала агентов и завершается с ненулевым кодом):

| Шаг | Действие | Проверка |
|---|---|---|
| a | PVC `e2e-r3` (1 ГиБ, `ambar-r3`) и под `w3` (образ `busybox` или другой уже имеющийся на узлах; закрепить по digest) на узле `dv-1` через `nodeSelector kubernetes.io/hostname`; под пишет файл `/data/f` и вызывает `sync` | PVC `Bound` за ≤ 120 с; под `Running`; объект `ReplicatedVolume` имеет три записи `Replica`, `initialSync: Done`, три записи `status.nodes` с `disk: UpToDate` |
| b | Размер устройства | `blockdev --getsize64 /dev/drbd<minor>` на `dv-1` не меньше 1073741824 |
| c | Удалить под, создать под `r3` на `dv-2` с тем же PVC | содержимое `/data/f` совпадает; на `dv-1` роль `Secondary` |
| d | PVC `e2e-r2` (`ambar-r2`) и под на `dv-1` | две записи `Replica` (одна из них `dv-1`), одна `Witness`, четвёртого узла в `spec.nodes` нет |
| e | Определить узел, которого нет в `spec.nodes` тома `e2e-r2`; удалить под, создать под на этом узле | в `spec.nodes` появилась запись `Client` этого узла; под `Running`; данные совпадают; на узле нет файла `<том>.img` |
| f | Удалить под с узла `Client` | запись `Client` исчезла из `spec.nodes` за ≤ 60 с; на узле `drbdsetup status <том>` сообщает, что ресурса нет; на узлах с копией статус не показывает соединения с этим узлом |
| g | Создать под на узле `Witness` | `spec.nodes` не изменился; под `Running`; данные совпадают. Удалить под: запись `Witness` на месте |
| h | Перезапустить DaemonSet `ambar` (`kc rollout restart`), пока под из шага a работает и пишет счётчик с `fsync` | счётчик растёт во время и после перезапуска; после перезапуска агенты записывают статус |
| i | PVC с классом, у которого `replicas: "1"` | PVC остаётся `Pending`, в событиях текст ошибки с `INVALID_ARGUMENT` |
| j | Удалить поды и PVC `e2e-r3`, `e2e-r2` | объектов `ReplicatedVolume` нет за ≤ 120 с; ни на одном узле нет файлов `*.img`, `*.res` этих томов, `drbdsetup status` пуст, `losetup -a` не показывает их |
| k | Отказ в удалении данных без объекта: создать том, убрать агент с одного узла с копией (временная метка-исключение в `nodeAffinity` DaemonSet), снять финализаторы объекта вручную и удалить объект, вернуть агент на узел | файл `<том>.img` на этом узле остался; в журнале агента строка уровня error с именем тома; после проверки убрать остатки командой `ambar remove --name <том>` в поде агента |

Шаг k проверяет R10 на живом кластере: объект удалён в обход финализатора, пока агент узла не работал. Способ временно убрать агент с одного узла исполнитель записывает в отчёт.

- [ ] **Step 1: параметризовать число узлов и поднять `dv-4`.**

Run: `NODES="dv-1 dv-2 dv-3 dv-4" stand/up.sh && NODES="dv-1 dv-2 dv-3 dv-4" stand/k3s.sh && NODES="dv-1 dv-2 dv-3 dv-4" stand/check.sh`
Expected: `kc get nodes` показывает четыре узла `Ready`; `stand/check.sh` завершается с кодом 0. Прежний запуск без переменной работает с тремя узлами.

- [ ] **Step 2: написать `stand/csi-e2e.sh`** (требует `NODES` из четырёх узлов; при трёх завершается с сообщением, что шаги e и f невыполнимы).

- [ ] **Step 3: прогнать дважды подряд.**

Run: `NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh && NODES="dv-1 dv-2 dv-3 dv-4" stand/csi-e2e.sh && NODES="dv-1 dv-2 dv-3 dv-4" stand/csi-e2e.sh`
Expected: оба прогона заканчиваются строкой `OK: csi e2e`; после второго на узлах нет остатков (проверка шага j).

- [ ] **Step 4: записать результаты.** В протокол — раздел «Проверки CSI на стенде»: таблица шагов с наблюдавшимися значениями (время до `Bound`, время исчезновения записи `Client`, вывод статуса после ухода стороны), число прогонов. В `stand/README.md` — запуск стенда из четырёх узлов, `stand/install.sh`, `stand/csi-e2e.sh`, ожидаемые последние строки.

- [ ] **Step 5: проверить, что проверки фазы 1 не сломаны.**

Run: `stand/install.sh --remove && stand/agent-smoke.sh`
Expected: `OK: agent smoke`.

- [ ] **Step 6: коммит.**

```bash
git add stand/csi-e2e.sh
git commit -m "test(stand): fourth VM and the end-to-end CSI check" -- stand docs/superpowers/specs/2026-11-12-drbd9-spike-results.md
```

---

### Task 15: спека, README, CI

**Files:**
- Modify: `docs/superpowers/specs/2026-11-12-ambar-design.md`
- Modify: `README.md`, `README.ru.md`
- Modify: `.github/workflows/ci.yml` (если `rust-version` поднят)

**Interfaces:**
- Consumes: решения R1–R17, отчёты задач 1–14.

- [ ] **Step 1: спека.** Привести к коду:
  - «Принятые решения»: строки для R1–R17 (решение и причина, как в таблице этого плана; ссылки на опыты P7–P9 и на проверки CSI).
  - «`StorageClass`»: параметр `maxPeers`; формулировка `replicas` без «остальные узлы подключаются».
  - «`ReplicatedVolume`»: пример с `kind`, `address`, `maxPeers`, `initialSync`; финализаторы по R9; правила неизменяемости; `status.nodes` как список; убрать абзац о прежней схеме и пометку от 2026-11-13.
  - «`ConfigMap ambar-nodes`»: ключ `nodes`, JSON; правило длины имени узла (1–55, последний символ — буква или цифра).
  - «Права агента (RBAC)»: права этой фазы; права на `pods` и `nodes` помечены как права фазы 3.
  - «Создание (`CreateVolume`)»: шаги по задаче 11, коды ошибок, топология (R16), свидетель, индекс `minor`/`port`.
  - «Маркеры на узле»: проверка `dump-md` перед `create-md`; условие `initialSync: Skip`.
  - «Монтирование»: членство `Client` в `NodeStageVolume`, правило признака файловой системы (таблица задачи 9) вместо `[проверить: способ чтения сигнатуры]`.
  - «Размонтирование»: уход `Client`, порядок по P8.
  - «Удаление (`DeleteVolume`)»: R9, R10, R11.
  - «Добавление узла в кластер»: запись в `ambar-nodes`; подключение по требованию; убрать шаги о добавлении узла во все тома.
  - «Чего агент фазы 1 не делает» → «Чего драйвер после фазы 2а не делает»: актуальный список (в том числе уборка членства `Client` без пода, смена набора копий, `StandAlone`).
  - «Структура репозитория»: фактические файлы.
  - «Открытые вопросы»: убрать закрытые (`address` и `maxPeers`); в строке о `--max-peers` указать принятое значение по умолчанию и что остаётся непроверенным.
  - Каждая пометка `[проверить: …]`, которую закрыл опыт этой фазы, заменяется ссылкой на опыт; незакрытые остаются.

- [ ] **Step 2: README.** В обоих файлах: строка состояния CSI-драйвера («создаёт, монтирует и удаляет тома на стенде; отказы узлов не обрабатываются»), раздел установки на стенд (ссылка на `stand/README.md`), строка о `deploy/` в таблице структуры, число тестов в ожидаемой строке `cargo test`, требование к версии Rust.

- [ ] **Step 3: проверка текста.** Поиск по изменённым документам слов из списка запрещённых в стиле документации и по всему дереву — приватных данных:

Run: поиск по дереву (кроме `target` и `.git`) шаблонов приватных данных; сами шаблоны контроллер передаёт исполнителю в задании, в репозиторий они не записываются.
Expected: совпадений нет (`rc=1`).

- [ ] **Step 4: полный гейт.**

Run: `cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked && image/test-loader.sh && docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`
Expected: всё проходит.

- [ ] **Step 5: коммит.**

```bash
git commit -m "docs: bring the spec and the READMEs in line with phase 2a" -- docs README.md README.ru.md .github/workflows/ci.yml
```

---

## Что остаётся после этого плана

| Что | Где будет сделано |
|---|---|
| Добавление, удаление и перенос копии | план 2б; до кода — опыты на стенде (diskless → diskful, удаление копии при `Primary` на другом узле) |
| Запись `Client` без пода после перезагрузки узла остаётся в `spec.nodes` | фаза 3 |
| Освобождение тома при потере кворума, taint, `StandAlone`, метрики | фаза 3 |
| Поведение при `ENOSPC` в файле тома | фаза 3, опыт на стенде |
| Замеры на R5C | фаза 4 |
