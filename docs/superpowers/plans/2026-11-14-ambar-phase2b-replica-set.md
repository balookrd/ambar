# ambar, фаза 2б: смена набора копий — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** у существующего тома можно добавить копию, удалить копию и перенести копию на другой узел командой `ambar replica`, не останавливая под тома; число копий не опускается ниже двух, данные удаляются только на узле, копию которого явно сняли.

**Архитектура:** оператор запускает `ambar replica add|remove|move` в контейнере агента (`kubectl exec`). Команда проверяет условия, правит `spec.nodes` объекта `ReplicatedVolume` JSON-патчем с проверкой прежнего списка и ждёт результата. Копию на узле создают и удаляют агенты в цикле согласования: новый вид участника `Retiring` означает «копию этого узла снять», агент узла удаляет её после проверок и сам убирает свою запись. Решения остаются чистыми функциями (`replicas`, `agent::decide`, `reconcile::plan`), правила переходов закреплены правилами валидации CRD.

**Стек:** Rust (edition 2024), `tokio`, `kube` 5.1.0, `k8s-openapi` 0.29.0, `clap`; DRBD 9.3.4, drbd-utils 9.35.0; стенд Lima с k3s v1.37.2.

**Спека:** `docs/superpowers/specs/2026-11-12-ambar-design.md`. **Протокол опытов:** `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`. **Стенд:** `stand/README.md`. **План фазы 2а (исполнен):** `docs/superpowers/plans/2026-11-13-ambar-phase2a-kubernetes.md`. **Журнал исполнения:** `.superpowers/sdd/progress.md` (строки этой фазы начинаются с `P2b`).

## Место этой фазы

| Фаза | Что даёт | Состояние |
|---|---|---|
| 0. Образ и стенд | модуль DRBD 9 в образе, стенд из ВМ Lima | исполнена |
| 1. Ядро агента | согласование одного тома на узле через CLI | исполнена |
| 2а. Kubernetes | CRD, цикл согласования, CSI, манифесты; том через PVC | исполнена |
| 2б. Смена набора копий (этот план) | добавление, удаление и перенос копии у существующего тома | план написан |
| 3. Отказы | освобождение тома при потере кворума, taint, `adjust` при `StandAlone`, метрики | план не написан |
| 4. Замер на R5C | память, `fsync`, сравнение с Longhorn | нужны узлы |

Что в этой фазе не делается: перенос копии на узел, где сейчас работает под тома (команда отказывает, см. R25); автоматическая перебалансировка при добавлении или выводе узла из кластера; смена `sizeBytes`, `maxPeers`, `protocol`; чтение метаданных файла без маркера готовности на пути удаления (решение владельца 2026-11-14: фаза 3).

## Решения владельца (2026-11-14)

| Вопрос | Решение |
|---|---|
| Как оператор просит изменить набор копий | Командой `ambar replica add|remove|move` в контейнере агента; команда берёт `nodeId` и адрес из `ConfigMap ambar-nodes`, проверяет условия и правит объект; CRD разрешает только эти переходы |
| Порядок переноса копии с узла A на узел B | Сначала добавить копию на B и дождаться `UpToDate`, затем снять копию с A. Число копий не опускается ниже N; на время переноса нужен бюджет на обоих узлах |
| Свидетель при смене чётности N | Команда сама держит правило «свидетель есть тогда и только тогда, когда N чётно»: назначает свидетеля, когда N становится чётным, и убирает, когда нечётным. Копию можно добавить на узел свидетеля, тогда он становится `Replica` |
| Снятие копии с узла, где работает под (`Primary`) | Команда отказывает; оператор сначала переносит под |

## Решения этого плана

Задача 11 переносит их в спеку. Нумерация продолжает R1–R18 фазы 2а.

| № | Решение | Причина |
|---|---|---|
| R19 | Вид участника `Retiring`: узел ещё держит копию, её нужно снять. Агент этого узла удаляет копию после проверок R26, затем сам убирает свою запись из `status.nodes`, снимает свой финализатор и последним шагом убирает свою запись из `spec.nodes`. Запись `Replica` нельзя удалить из `spec.nodes` иначе, чем через `Retiring` | R10 запрещает удалять данные по исчезновению записи: ошибочная правка `spec.nodes` не должна стирать копию. `Retiring` — явное намерение, отличимое от ошибки |
| R20 | Правила переходов CRD (заменяют правила «the set of Replica and Witness nodes is immutable» и «a member node cannot change nodeId, address or kind»): у записи с тем же именем не меняются `nodeId` и адрес; вид меняется только по переходам `Witness → Replica`, `Client → Replica`, `Replica → Retiring`, `Client → Witness`, `Witness → Client`; при изменении объекта новая запись — любого вида, кроме `Retiring`; исчезнуть может только запись вида `Retiring`, `Witness` или `Client`; набор записей `Replica`, `Retiring`, `Witness` меняется только при `initialSync: Done` в прежнем и новом объекте. Правило для создания и изменения: записей `Retiring` не больше одной. При создании переходные правила не действуют: объект, созданный вручную с `Retiring`, получит `FinishRetire` на узле без данных; `CreateVolume` `Retiring` не создаёт | Каждый переход, который делает команда, разрешён; удаление копии в обход `Retiring` и смена роли без создания метаданных — нет |
| R21 | `to_volume_spec` переводит `Retiring` в узел с копией (`diskful: true`): пока копия не снята, сторона остаётся сторон с диском у всех участников. Лидер пропуска синхронизации выбирается среди `Replica` | Узел, снимающий копию, до `down` — обычная сторона с диском; пропуск синхронизации при `Retiring` невозможен по R20 |
| R22 | Добавление (`ambar replica add <том> <узел>`): первый патч делает узел `Replica` (новая запись или переход из `Witness`/`Client`) и добавляет финализатор `replica-<узел>`. Тот же патч назначает свидетеля (R27), если его нет после правки, а он был до неё (узел аргумента — бывший свидетель) или новое N чётно. Команда ждёт `disk: UpToDate` у записи узла в `status.nodes`. Затем, если N нечётно и свидетель есть, второй патч убирает свидетеля (R27) | Свидетель остаётся до конца синхронизации: пока новая копия `Inconsistent`, она может не давать кворума `[проверить: опыт P14]`; лишний участник кворуму не мешает. Добавление копии на узел свидетеля без замены оставило бы том на время `down` и полной синхронизации с двумя сторонами, дающими кворум |
| R23 | Снятие (`ambar replica remove <том> <узел>`): патч переводит узел в `Retiring`; если новое N чётно и свидетеля нет, тот же патч назначает свидетеля. Команда ждёт, пока запись узла исчезнет из `spec.nodes`. Затем второй патч доводит правило свидетеля: N нечётно и свидетель есть — свидетель убирается; N чётно и свидетеля нет (при первом патче кандидата не было) — свидетель назначается, и снятый узел теперь кандидат | Новый свидетель подключается до `down` снимаемого узла: агент снимаемого узла сначала приводит свой ресурс к описанию (R26), затем ждёт соединения со всеми участниками (правило 3) |
| R24 | Перенос (`ambar replica move <том> <с узла> <на узел>`) — свой порядок шагов (`replicas::next_move`), чётность N до и после одна, поэтому свидетель не меняется: первый патч делает целевой узел `Replica` с финализатором (если целевой узел — свидетель, тот же патч назначает нового свидетеля по R27); ожидание `UpToDate`; второй патч переводит исходный узел в `Retiring`; ожидание исчезновения его записи. Повторный запуск продолжает с того шага, на котором остановился: исходный узел уже `Retiring` или ушёл — шаги добавления пропускаются | Прерванная команда оставляет N+1 копий или незаконченное снятие; оба состояния агенты доводят сами, команда при повторе их узнаёт. Свидетель не отключается и не подключается заново |
| R25 | Условия первого патча любой подкоманды: объект есть и не удаляется; `initialSync` равен `Done`; записи `Retiring` нет; у каждого узла `Replica` из `spec.nodes`, кроме узла, который снимается этой командой, есть запись в `status.nodes` с `disk: UpToDate`; у каждого участника, кроме снимаемого, есть запись, и в ней соединение с каждым другим участником, кроме снимаемого, в состоянии `Connected`, а с каждым `Replica` — ещё и `peerDisk: UpToDate`; узлы аргументов есть в `ambar-nodes`. `add`: если узел не участник, после патча участников не больше `maxPeers + 1`; свободного бюджета узла (как в `placement`) хватает на `sizeBytes`; узел `Witness` или `Client` в `status.nodes` не `Primary`. `remove`: узел `Replica`; после снятия остаётся не меньше двух `Replica`; узел в `status.nodes` не `Primary`. Узел, который уже `Replica` (`add`, целевой узел `move`), — продолжение начатой команды, а не отказ: команда ждёт его `UpToDate` и доводит правило свидетеля. Условия «нет `Retiring`» и «все `UpToDate`» после первого патча не проверяются. `remove` узла, который не участник и чей финализатор снят, — отказ `NotAMember` без правок. Отказ — код выхода 1 и текст причины, объект не меняется | Одна смена за раз; данные не убывают ниже двух копий; узел с работающим подом не меняет роль (решение владельца). Недосинхронизированную копию можно снять: требование `UpToDate` не распространяется на снимаемый узел. Запись выключенного узла в `status.nodes` никто не обновляет, и она показывает прежнее `UpToDate`; соединения в записях живых узлов показывают его несоединённым, поэтому проверка соединений «каждый с каждым» отказывает сразу, а не оставляет узел в `Retiring` в ожидании, которое нельзя отменить (CRD запрещает `Retiring → Replica`) |
| R26 | Снятие копии агентом узла (`Task::RetireReplica`). Ресурс не настроен в ядре: те же строки, что у удаления (подъём копии с маркером `.img.ready`, доказанные случаи «данных нет», остальное удерживается). Ресурс настроен, но файл ресурса не совпадает с описанием, не применён или статус не совпадает с описанием (`status_matches_spec`), — сначала `Reconciler::apply` с описанием (`Retiring` с диском, `initialSync: Done`), исход `Progress`, ничего не удаляется. Ресурс настроен и совпадает с описанием: копия удаляется, только если (1) локальный статус DRBD прочитан; (2) этот узел не `Primary`; (3) каждый другой участник `spec.nodes` любого вида соединён (`Connected`); (4) у каждого другого `Replica` в локальном статусе `peer-disk` равен `UpToDate`. Проверки повторяются под `flock` тома (guard `remove_checked`). Затем по порядку: удаление копии (`remove_checked`), удаление записи `status.nodes` с проверкой (`clear_entry`), снятие финализатора, удаление своей записи из `spec.nodes` (`replace_members`). Сторона в роли `Primary` снятию не мешает | В отличие от удаления тома, под на другом узле продолжает работать: снимается одна копия, остальные `UpToDate` и соединены. Без `apply` на снимаемом узле участник, добавленный тем же патчем (новый свидетель) или позже (`Client`), не появился бы в его конфигурации DRBD, и правило 3 не выполнилось бы никогда. Правило 3 держит кворум: узел уходит, только когда все остальные на месте `[проверить: опыт P12]`. Финализатор снимается раньше записи: если агент завершится между шагами, узел остаётся `Retiring` без данных и без финализатора, и следующий проход (`FinishRetire`) убирает запись |
| R27 | Выбор свидетеля: среди узлов `ambar-nodes`, которые не `Replica` и не `Retiring` и не узел аргумента команды, пока этот узел участник тома, сначала участник вида `Client` (переход `Client → Witness` не меняет DRBD), иначе узел вне тома с наибольшим свободным бюджетом, при равенстве — по имени. Нового участника берут, только если после патча участников не больше `maxPeers + 1`. Нет подходящего узла — свидетеля нет (как в `placement::place`). В кластере из трёх узлов снятие копии с тома `replicas: 3` не находит кандидата при первом патче (два узла — `Replica`, третий снимается); свидетелем становится снятый узел после ухода из тома (R23, второй патч). До этого том держат две копии `UpToDate` без свидетеля: отказ одного из двух узлов с копией в этом окне лишает том кворума Удаление свидетеля: если свидетель в `status.nodes` `Primary` (на узле работает под), он переходит в `Client`, иначе запись удаляется | Переиспользует правило `placement::place`; свидетель с подом не теряет подключение |
| R28 | Узел `Witness` или `Client`, ставший `Replica`, получает копию так: если локальная роль `Primary` — проход останавливается ошибкой `PlanError::ConvertWhilePrimary` (повтор через 5 с); иначе `Down`, затем обычные шаги узла с копией (`CreateFile`, `AttachLoop`, `LinkDevice`, `WriteRes`, `CreateMd`, `MarkReady`, `Up`, `MarkApplied`). Полную синхронизацию с остальных копий DRBD выполняет сам (`initialSync: Done`) | `create-md` под настроенным ресурсом запрещён правилом фазы 1 (`UninitialisedFileWithLiveResource`); `down` у стороны без диска в роли `Secondary` данных не трогает. Поведение проверяет опыт P11 |
| R29 | `drbdsetup forget-peer` агент не выполняет | `nodeId` узла постоянный (R4), поэтому вернувшаяся сторона получает свой прежний слот битовой карты. Слотов не хватит, только если за жизнь тома к нему подключится больше `maxPeers` разных `nodeId`, то есть кластер вырастет больше `maxPeers + 1` узлов. `forget-peer` после каждого `adjust`, убравшего сторону, выполнялся бы при каждом уходе `Client` (каждом переезде пода), и при ошибке не повторялся бы: на следующем проходе соединения уже нет. Опыт P13 записывает, что происходит при нехватке слотов; ограничение идёт в спеку (задача 11) |
| R30 | Агент не создаёт копию без своего финализатора: объект не удаляется, узел `Replica`, финализатора `replica-<узел>` нет, файла тома нет — отказ `Refusal::ReplicaWithoutFinalizer` | Копию без финализатора нельзя удалить штатно (R10, `DeletingWithoutFinalizer`); команда добавляет финализатор тем же патчем, что и запись |
| R31 | Бюджет узла (`placement`) считает `Retiring` как `Replica`; N в правиле свидетеля — число записей `Replica` без `Retiring`. `NodeStageVolume` на узле `Retiring` отвечает `UNAVAILABLE` с текстом «the copy on this node is being removed» | Файл снимаемой копии занимает место до удаления; под на снимаемом узле сделал бы его `Primary` и остановил снятие (R26, правило 2). `UNAVAILABLE` повторяется kubelet, после ухода записи узел подключается как `Client` |

## Global Constraints

- Версия ядра: `6.12.116+deb13-arm64`. DRBD 9.3.4, drbd-utils 9.35.0. Версии не менять. Версии крейтов закреплены в `Cargo.lock`; новых зависимостей план не добавляет. Перед использованием API крейта исполнитель сверяется с его документацией для закреплённой версии.
- Правила безопасности данных фаз 1 и 2а действуют без изменений (шапки `src/reconcile.rs`, `src/agent/run.rs`, `src/agent/decide.rs`): `create-md` только после `dump-md` с результатом `Absent` или `Fresh`; `new-current-uuid --clear-bitmap` только лидером при `initialSync: Skip`; `drbdadm primary --force` не вызывается; результат команд `drbdadm` проверяется повторным чтением статуса; копия удаляется только через `Reconciler::remove_checked` или `remove_unconfigured` с guard под `flock`; R10 и R18 для удаления тома не ослабляются.
- Новые пути к удалению данных этой фазы — только `Task::RetireReplica` (R26). Других путей нет; таблица путей в шапке `src/agent/run.rs` дополняется строками этого пути.
- Группа API: `ambar.balookrd.github.io`, версия `v1alpha1`, вид `ReplicatedVolume`. Пространство имён: `ambar-system`. Финализатор: `ambar.balookrd.github.io/replica-<узел>`.
- Пути на узле: тома `/var/lib/rancher/ambar/volumes`, файлы ресурсов `/var/lib/rancher/ambar/res` (в контейнере `/etc/drbd.d`), сокет CSI `/var/lib/kubelet/plugins/ambar/csi.sock`.
- Вызовы API ограничены `API_CALL_TIMEOUT` (30 с) через `agent::kube::bounded`; новые вызовы — тоже.
- Тесты лежат в `tests/` рядом с модулем и подключаются через `#[cfg(test)] #[path = "tests/<имя>.rs"] mod tests;`. Inline-блоков `mod tests { … }` нет. Общие фейки — `src/tests/volumes.rs`.
- TDD: тест пишется первым и запускается до изменения кода; отчёт задачи говорит, что каждый новый тест падал до изменения. Для правил безопасности — проверка мутацией: удаление условия из кода роняет хотя бы один тест; отчёт перечисляет условие и тест.
- Гейт перед каждым коммитом: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `image/test-loader.sh`. Тот же `cargo test --locked` в контейнере Linux (`docker run --rm -e CARGO_TARGET_DIR=/tmp/tgt -v "$PWD":/src -w /src rust:1-slim cargo test --locked`) с тем же числом тестов. Перед задачами со стендом — `cargo zigbuild --release --locked --target aarch64-unknown-linux-musl` и `image/build.sh`.
- Комментарии в коде и сообщения коммитов — на английском, тема коммита не длиннее 72 символов, тело переносится на 72. Документы в `docs/superpowers/**` — на русском, в стиле технической документации (факты, таблицы параметров, `[проверить: …]` для непроверенного). `README.md` и `README.ru.md` правятся вместе.
- Репозиторий публичный. В файлы и сообщения коммитов не попадают адреса домашней сети владельца, его домашний домен и путь домашнего каталога; адреса стенда `192.168.104.x` и адреса из `192.0.2.0/24` в примерах допустимы.
- `git commit -- <пути>` локально в `main`, без веток, без трейлера `Co-Authored-By` и подписей об авторстве. Запрещены `git stash`, `git reset`, `git checkout -- …`, `git add -A`, `git commit -a`. `git push` — только по отдельной команде владельца.
- Утверждение в документах о поведении DRBD или Kubernetes допускается только со ссылкой на опыт из протокола либо с пометкой `[проверить: …]`.
- Стенд: ВМ `dv-1..4`, `NODES="dv-1 dv-2 dv-3 dv-4"`. `stand/down.sh` удаляет ВМ — не запускать. `stand/install.sh --remove` не запускать. После задачи стенд остаётся: четыре ВМ, драйвер установлен, томов и опытных ресурсов нет (`stand/check.sh` проходит).

## Структура файлов

| Файл | Ответственность | Задача |
|---|---|---|
| `stand/spike/pre-2b.sh` | опыты P11–P14 | 1 |
| `src/crd.rs`, `deploy/crd.json`, `src/tests/crd.rs` | вид `Retiring`, правила переходов, `to_volume_spec` | 2 |
| `src/model.rs` | лидер пропуска синхронизации среди `Replica` | 2 |
| `src/placement.rs`, `src/csi/controller.rs` | бюджет с `Retiring`, `existing_of` в `placement`, выбор свидетеля как отдельная функция | 3 |
| `src/reconcile.rs` | `Down` при смене роли | 4 |
| `src/agent/decide.rs`, `src/agent/run.rs` (временная ветка) | строки `Retiring`, `ReplicaWithoutFinalizer` | 5 |
| `src/agent/run.rs`, `src/agent/kube.rs` | исполнение `RetireReplica`, `FinishRetire` | 6 |
| `src/csi/node.rs` | `NodeStageVolume` на узле `Retiring` | 7 |
| `src/replicas.rs` (новый) | чистые правила команды: условия, патчи, свидетель | 8 |
| `src/agent/replica_cmd.rs` (новый), `src/main.rs` | команда `ambar replica`: чтение, патч, ожидание | 9 |
| `stand/replica-e2e.sh` (новый) | сквозная проверка на стенде | 10 |
| спека, протокол, `README.md`, `README.ru.md`, `stand/README.md` | документы | 11 |

---

### Task 1: опыты на стенде перед кодом (P11–P14)

Кода агента в задаче нет. P11 и P12 подтверждают или отменяют R26 и R28 (задачи 4 и 6 на них ссылаются); P13 и P14 дают записи для ограничений R29 и R22 в спеке.

**Files:**
- Create: `stand/spike/pre-2b.sh` (по образцу `stand/spike/pre-csi.sh`: `source ../lib.sh`, функции `cleanup`, `write_res`, `backing`, `run_rc`, писатель `probe.sh` и наблюдатель `watch.sh`, выбор опыта аргументом `p11|p12|p13|p14|clean`)
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` (новый раздел «Опыты перед сменой набора копий (P11–P14)»)
- Modify: `stand/README.md` (строка о скрипте рядом со строкой о `pre-csi.sh`)

**Interfaces:**
- Produces: записанные в протоколе (а) порядок команд смены роли «без диска → с копией» в двух порядках и время до `UpToDate`; (б) поведение кворума и писателя при уходе стороны с диском до и после `adjust` остальных и при уходе стороны без диска в обратном порядке; (в) код возврата и вывод `drbdsetup forget-peer` и поведение слотов битовой карты; (г) кворум при синхронизирующейся копии `Inconsistent`. Задачи 4, 6 и 11 ссылаются на эти записи.

Опыты запускаются на стенде с установленным драйвером в поде `toolbox` (`tb <узел> "<команда>"` из `stand/lib.sh`); ресурсы `p11`–`p14` (minor 1011–1014, port 7011–7014) не пересекаются с томами драйвера (minor от 1000, на стенде томов нет). Перед опытами `kubectl get rv` пуст.

- [ ] **Step 1: P11, сторона без диска становится стороной с копией.** Ресурс `p11`: `dv-1`, `dv-2` с копией (файлы 1 ГиБ, `create-md --force --max-peers=7`), `dv-3` без диска (`disk none`), начальная синхронизация пропущена (`new-current-uuid --clear-bitmap p11/0` на `dv-1`), на `dv-1` ext4 смонтирована, работают `probe.sh` и `watch.sh`; на `dv-1` раз в секунду пишется `quorum` из `drbdsetup status p11 --json` в журнал. Затем:
  1. На `dv-3` при поднятом ресурсе без диска: создать файл 1 ГиБ, `losetup --direct-io=on`, переписать `p11.res` с диском на `dv-3`, выполнить `drbdadm create-md --force --max-peers=7 p11`; записать код возврата и вывод (ожидается отказ: ресурс настроен).
  2. На `dv-3`: `drbdadm down p11`; записать статус `dv-1` и `dv-2` (`drbdsetup status p11 --verbose`).
  3. На `dv-3`: `drbdadm create-md --force --max-peers=7 p11`, `drbdadm up p11`; записать коды возврата.
  4. На `dv-1` и `dv-2`: записать `p11.res` с диском на `dv-3`, `drbdadm adjust p11`; записать коды возврата и статус.
  5. Опрашивать `drbdsetup status p11 --verbose --statistics` на `dv-3` раз в 5 с до `UpToDate`; записать время и `out-of-sync`.
  6. Остановить писателя; сравнить подтверждённый номер и номер на томе; записать журнал кворума и `dmesg -T | grep 'drbd p11'` трёх узлов.
  7. Второй прогон в порядке, который дают независимые агенты: `clean`, сборка `p11` заново; сначала на `dv-1` и `dv-2` записать `p11.res` с диском на `dv-3` и выполнить `drbdadm adjust p11`, пока `dv-3` работает без диска; записать коды возврата, статус трёх узлов и журнал кворума; затем на `dv-3` пункты 2 и 3 (без пункта 4); записать то же и время до `UpToDate`.
  Вопросы: отказывает ли `create-md` при поднятом ресурсе без диска; проходит ли полная синхронизация на `dv-3` при писателе на `dv-1`; прерывалась ли запись; был ли `quorum` всё время `true`; нужен ли `adjust` на `dv-1` и `dv-2` и что он меняет; работают ли оба порядка (сначала смена роли на `dv-3`, сначала `adjust` у остальных).

- [ ] **Step 2: P12, уход стороны с копией.** Ресурс `p12`: `dv-1`, `dv-2`, `dv-3` с копией, `--max-peers=7`, синхронизация пропущена, писатель и журнал кворума на `dv-1`. Затем:
  1. На `dv-3`: `drbdadm down p12`. Записать статус `dv-1` и `dv-2`, журнал кворума за 30 с (ожидается `quorum: true`: две из трёх сторон с диском на месте).
  2. На `dv-1` и `dv-2`: `p12.res` без `dv-3`, `drbdadm adjust p12`; записать коды возврата, статус, журнал кворума.
  3. На `dv-1`: `drbdsetup forget-peer p12 2` (`node-id` `dv-3`); записать код возврата и вывод. Повторить ту же команду; записать код возврата и вывод (сторона уже забыта).
  4. На `dv-2`: та же команда для стороны, которой нет в метаданных (`node-id` 5); записать код возврата и вывод.
  5. Остановить писателя, сравнить номера.
  6. Уход стороны без диска в обратном порядке (так убирают свидетеля и `Client` агенты): ресурс `p12` заново на `dv-1`, `dv-2` с копией и `dv-4` без диска, писатель на `dv-1`. На `dv-1` и `dv-2` `p12.res` без `dv-4`, `drbdadm adjust p12`, пока ресурс на `dv-4` поднят и соединён; записать коды возврата, статус трёх узлов, журнал кворума. Затем на `dv-4` `drbdadm down p12`; записать то же.
  Вопросы: держится ли кворум при уходе одной из трёх сторон с диском до и после `adjust` остальных; что печатает `forget-peer` в каждом случае; убирают ли остальные сторону без диска `adjust`ом, пока она поднята, и что видит она сама.

- [ ] **Step 3: P13, слоты битовой карты при смене сторон.** Ресурс `p13` с `--max-peers=1`: `dv-1` и `dv-2` с копией, синхронизация пропущена, на `dv-1` записаны 64 МиБ (`dd if=/dev/urandom of=/dev/drbd1013 bs=1M count=64 oflag=direct`).
  1. Снять `dv-2` (`down` на `dv-2`; `p13.res` на `dv-1` без `dv-2`; `adjust` на `dv-1`). Без `forget-peer`.
  2. Добавить `dv-3` с копией (`node-id` 2): файл, `create-md --force --max-peers=1`, `up` на `dv-3`; `p13.res` на `dv-1` с `dv-3`, `adjust` на `dv-1`. Записать коды возврата, статус на обоих узлах через 30 с, `dmesg -T | grep 'drbd p13'` на `dv-1` (ожидается отказ в слоте битовой карты; если соединение и синхронизация прошли — это и записать).
  3. Вернуть исходное состояние (`clean`), повторить пункты 1–2 с `drbdsetup forget-peer p13 1` на `dv-1` после пункта 1. Записать то же.
  4. После синхронизации в любом из прогонов сравнить `sha256sum` первых 64 МиБ `/dev/drbd1013` на `dv-1` и на `dv-3` (ресурс на `dv-3` `Secondary`, чтение с `dv-3` после `drbdadm primary p13` на `dv-3` при `secondary` на `dv-1`; ext4 не создаётся).
  Вопросы: занимает ли ушедшая сторона слот до `forget-peer`; освобождает ли его `forget-peer`; доходит ли новая сторона до `UpToDate` с теми же данными.

- [ ] **Step 4: P14, кворум при синхронизирующейся копии.** Ресурс `p14`: `dv-1`, `dv-2` с копией, `dv-4` без диска (свидетель), файлы 4 ГиБ, синхронизация пропущена, на `dv-1` записаны 2 ГиБ (`dd … count=2048 oflag=direct`), писатель `probe.sh` и журнал кворума на `dv-1`. Добавить `dv-3` с копией (порядок из P11 для нового узла: файл, `create-md`, `up` на `dv-3`; `adjust` на остальных). Пока `dv-3` `Inconsistent` (проверить по статусу), выполнить `drbdadm down p14` на `dv-2`. Записать журнал кворума на `dv-1`, ошибки писателя, статус. Вернуть `dv-2` (`up`), дождаться `UpToDate` всех копий. Повторить без свидетеля: `p14.res` без `dv-4` на всех, `down` на `dv-4`, снова новая копия на `dv-3` (`clean` и сборка заново), `down` на `dv-2` во время синхронизации. Два прогона каждого варианта.
  Вопросы: сохраняет ли `dv-1` кворум со свидетелем и без него, когда третья сторона с диском `Inconsistent`.

- [ ] **Step 5: записать результаты.** Раздел протокола «Опыты перед сменой набора копий (P11–P14)»: таблица «опыт | условия | команда | код возврата | вывод | число прогонов», под ней сырой вывод в блоках кода. Вывод о поведении — в пределах записанного, с числом прогонов. Адреса в выводе — только `192.168.104.x`.

- [ ] **Step 6: проверить чистоту стенда и закоммитить.**

Run: `NODES="dv-1 dv-2 dv-3 dv-4" stand/spike/pre-2b.sh clean && NODES="dv-1 dv-2 dv-3 dv-4" stand/check.sh`
Expected: код 0; `tb dv-1 "drbdsetup status"` не печатает ресурсов `p11`–`p14`; файлов `p1[1-4]*` нет в каталоге опытов.

```bash
git commit -m "docs(spike): replica set change checks P11-P14" -- stand/spike/pre-2b.sh stand/README.md docs/superpowers/specs/2026-11-12-drbd9-spike-results.md
```

Исполнитель останавливается и сообщает контроллеру, если: в P11 в любом из двух порядков копия на `dv-3` не дошла до `UpToDate` или запись прервалась; в P12 кворум на `dv-1` пропал или сторона без диска не убралась `adjust`ом остальных; в P13 хэши различаются. От этих результатов зависят R26 и R28 и задачи 4 и 6; решение о плане принимает владелец. Результаты P13 (слоты) и P14 (кворум при `Inconsistent`) сами по себе план не останавливают: R29 не выполняет `forget-peer`, R22 и R24 сохраняют свидетеля; они записываются в ограничения спеки.

---

### Task 2: CRD — вид `Retiring` и правила переходов

**Files:**
- Modify: `src/crd.rs` (`NodeKind`, `x-kubernetes-validations` на `ReplicatedVolumeSpec`, `to_volume_spec`)
- Modify: `src/model.rs` (`VolumeNode`, `initial_sync_leader`)
- Modify: `deploy/crd.json` (вывод `cargo run --locked -- crd`)
- Test: `src/tests/crd.rs`, `src/tests/model.rs`

**Interfaces:**
- Produces: `NodeKind::Retiring` (сериализуется как `"Retiring"`); `VolumeNode` получает поле `retiring: bool` (истинно для `Retiring`, у `Replica` ложно; `diskful` истинно у обоих); `VolumeSpec::initial_sync_leader` выбирает наименьший `node_id` среди `diskful && !retiring`. Задачи 3–9 используют эти имена.

- [ ] **Step 1: тесты `to_volume_spec`.** В `src/tests/crd.rs`: объект с `Replica`, `Replica`, `Retiring` даёт три узла `diskful`, у последнего `retiring == true`; объект с одним `Replica` и одним `Retiring` проходит `VolumeSpec::validate`: правило `MIN_DISKFUL` считает узлы с `diskful` (`src/model.rs`, `validate`), `Retiring` входит в счёт. Двух `Replica` требует CRD, а агенту описание нужно до конца снятия. В `src/tests/model.rs`: лидер — наименьший `node_id` среди `Replica`, `Retiring` с меньшим `node_id` лидером не становится. Запустить: `cargo test --locked crd model` — тесты не компилируются (нет варианта и поля).

- [ ] **Step 2: реализация.** Вариант `Retiring` с doc-комментарием «Holds a copy that is being removed; the node's agent deletes it and then removes this entry»; поле `retiring` в `VolumeNode`; `to_volume_spec`: `diskful: matches!(kind, Replica | Retiring)`, `retiring: kind == Retiring`; `initial_sync_leader` с фильтром. Литерал `VolumeNode` вне тестов один — в `to_volume_spec` (`src/crd.rs`); литералы в тестах получают `retiring: false`. `VolumeNode` читается из JSON в `ambar apply --file` с `deny_unknown_fields`; поле получает `#[serde(default)]`, чтобы файлы фазы 1 без него читались. Тест: JSON фазы 1 без поля `retiring` читается, `retiring == false`.

- [ ] **Step 3: правила CRD.** Заменить второе и третье правило списка (сейчас «the set of Replica and Witness nodes is immutable» и «a member node cannot change nodeId, address or kind») правилами R20. Текст правил и сообщений:

| Правило CEL | Сообщение |
|---|---|
| `self.nodes.all(n, oldSelf.nodes.all(o, o.name != n.name \|\| (o.nodeId == n.nodeId && o.address == n.address)))` | `a member node cannot change nodeId or address` |
| `self.nodes.all(n, oldSelf.nodes.all(o, o.name != n.name \|\| o.kind == n.kind \|\| (o.kind == 'Witness' && n.kind == 'Replica') \|\| (o.kind == 'Client' && n.kind == 'Replica') \|\| (o.kind == 'Replica' && n.kind == 'Retiring') \|\| (o.kind == 'Client' && n.kind == 'Witness') \|\| (o.kind == 'Witness' && n.kind == 'Client')))` | `this change of a node's kind is not allowed` |
| `self.nodes.all(n, n.kind != 'Retiring' \|\| oldSelf.nodes.exists(o, o.name == n.name))` | `a node can become Retiring only from Replica` |
| `oldSelf.nodes.all(o, o.kind == 'Client' \|\| o.kind == 'Witness' \|\| o.kind == 'Retiring' \|\| self.nodes.exists(n, n.name == o.name))` | `a Replica node is removed only through Retiring` |
| `(self.nodes.filter(n, n.kind != 'Client').map(n, n.name + '/' + n.kind) == oldSelf.nodes.filter(o, o.kind != 'Client').map(o, o.name + '/' + o.kind)) \|\| (self.initialSync == 'Done' && oldSelf.initialSync == 'Done')` | `the set of Replica, Retiring and Witness nodes changes only when initialSync is Done` |

Новое правило создания и изменения: `self.nodes.filter(n, n.kind == 'Retiring').size() <= 1`, сообщение `at most one node can be Retiring`. Правило «at least two Replica nodes are required» не меняется (`Retiring` в счёт не входит). Сравнение списков в пятом правиле опирается на сортировку `spec.nodes` по имени (так пишут `placement` и `membership`; команда задачи 9 сохраняет сортировку); исполнитель проверяет на стенде (шаг 4a), что `map` над списком принимается бюджетом стоимости CEL; если API-сервер отклоняет CRD из-за стоимости, правило переписывается через `all`/`exists` с тем же смыслом, и отчёт об этом говорит. Комментарий над списком правил обновить: какие правила переходные, какие действуют и при создании.

- [ ] **Step 4: тесты правил.** В `src/tests/crd.rs` уже есть проверки текста правил (посмотреть, как устроены; если они сравнивают `deploy/crd.json` с выводом `crd_json()`, перегенерировать файл). Добавить тест, что в CRD ровно тринадцать правил (девять прежних минус два заменённых, плюс пять переходных из таблицы, плюс правило о числе `Retiring`) с перечисленными сообщениями и что `NodeKind` в схеме имеет четыре значения.

- [ ] **Step 4a: правила на API-сервере стенда.** До коммита: `kc apply --dry-run=server -f deploy/crd.json` — без ошибок (в том числе без ошибки бюджета стоимости CEL); затем `kc apply -f deploy/crd.json` (CRD без объектов на стенде; агенты фазы 2а со старым `NodeKind` объектов с `Retiring` не увидят). На тестовом объекте `rv-cel-check` (две `Replica` на `dv-1`, `dv-2`, `Witness` на `dv-3`, `initialSync: Done`, без финализаторов, `minor` 1999, `port` 7999; агенты применят его, поэтому объект создаётся при остановленном DaemonSet: `kc -n ambar-system patch ds ambar -p '{"spec":{"template":{"spec":{"nodeSelector":{"ambar-cel-check":"none"}}}}}'`, после проверки патч снимается тем же способом и объект удаляется) каждая правка через `kc patch rv rv-cel-check --type=json --dry-run=server`: разрешённые — новая `Replica`, новый `Witness`, `Witness → Replica`, `Client → Replica`, `Replica → Retiring`, `Client → Witness`, `Witness → Client`, удаление `Witness`, удаление `Client`, удаление `Retiring`; запрещённые (сообщение проверяется `grep`) — удаление `Replica`, `Replica → Client`, новая запись `Retiring`, смена `nodeId`, два `Retiring`, новая `Replica` при `initialSync: Skip` (отдельный объект со `Skip`). Результаты — таблицей в отчёт задачи. Если сервер отклоняет CRD по стоимости, правило переписывается (шаг 3) и проверка повторяется. Стенд после шага: DaemonSet без `nodeSelector`, все четыре пода `Ready`, объектов `rv` нет.

- [ ] **Step 5: гейт и коммит.**

Run: `cargo run --locked -- crd > deploy/crd.json && cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: всё проходит; `git diff --stat` показывает только файлы задачи.

```bash
git commit -m "feat(crd): Retiring kind and replica set transition rules" -- src/crd.rs src/model.rs deploy/crd.json src/tests/crd.rs src/tests/model.rs <другие изменённые файлы с литералами VolumeNode>
```

---

### Task 3: бюджет и выбор свидетеля в `placement`

**Files:**
- Modify: `src/placement.rs`
- Modify: `src/csi/controller.rs` (`existing_of`)
- Test: `src/tests/placement.rs`, `src/csi/tests/controller.rs`

**Interfaces:**
- Consumes: `NodeKind::Retiring` (задача 2).
- Produces: `pub fn existing_of(object: &ReplicatedVolume) -> Existing` переносится из `src/csi/controller.rs` в `src/placement.rs` (контроллер вызывает её оттуда; тест переезжает в `src/tests/placement.rs`); `pub fn free_budgets<'a>(nodes: &'a ClusterNodes, existing: &[Existing]) -> HashMap<&'a str, u64>` (сейчас приватная, сигнатура та же); `pub fn pick_witness(nodes: &ClusterNodes, free: &HashMap<&str, u64>, exclude: &[&str], clients: &[&str]) -> Option<String>` — первый по имени из `clients`, не входящий в `exclude`; иначе узел из `nodes` вне `exclude` и вне `clients` с наибольшим свободным бюджетом, при равенстве — по имени; `None`, если такого нет. `place` вызывает `pick_witness` для шага 4 с пустым `clients` (поведение `place` не меняется). Задача 8 использует обе функции.

- [ ] **Step 1: тесты.** `placement::existing_of` объекта с `Replica` на `a`, `Retiring` на `b`, `Witness` на `c` даёт `replica_nodes == ["a", "b"]` (тест в `src/tests/placement.rs`). `pick_witness`: клиент выбирается раньше узла с большим бюджетом; клиент из `exclude` не выбирается; без клиентов — наибольший бюджет, при равенстве меньшее имя; все узлы исключены — `None`. Существующие тесты `place` проходят без изменений. Запустить, увидеть падение.

- [ ] **Step 2: реализация.** `existing_of` переносится в `placement`, фильтр `kind == Replica || kind == Retiring`. `pick_witness` и публичная `free_budgets`; шаг 4 `place` через `pick_witness`.

- [ ] **Step 3: гейт и коммит.**

```bash
git commit -m "feat(placement): count Retiring in budgets, expose witness choice" -- src/placement.rs src/csi/controller.rs src/tests/placement.rs src/csi/tests/controller.rs
```

---

### Task 4: `reconcile` — смена роли на узле

**Files:**
- Modify: `src/reconcile.rs` (`Action`, `PlanError`, `plan`, исполнитель, шапка модуля)
- Test: `src/tests/reconcile.rs`

**Interfaces:**
- Consumes: протокол P11 (задача 1); `VolumeNode::retiring` (задача 2).
- Produces: `Action::Down` (выполняет `drbdadm down <том>` через существующий `Drbd`; после неё статус перечитывается, ресурс должен отсутствовать, иначе `NotApplied`); `PlanError::ConvertWhilePrimary { volume }` с текстом «volume {volume:?}: this node becomes a Replica but is Primary; the copy is created after the volume is unmounted here».

- [ ] **Step 1: сверить порядок с P11.** Порядок действий ниже — `down`, `create-md`, `up` на узле, ставшем копией, — проверен P11 в обоих порядках относительно `adjust` остальных. Если протокол показал иной рабочий порядок, исполнитель останавливается и сообщает контроллеру (условие остановки задачи 1).

- [ ] **Step 2: тесты `plan`.** Новые строки таблицы решений (вписать их и в шапку модуля). «Без диска» здесь — `device.client` (сторона настроена без диска), а не `disk_state == Diskless`: у копии, чей диск отсоединился после ошибки ввода-вывода, диск тоже `Diskless`, но `client` ложно.

| Вход | Результат |
|---|---|
| узел с копией по описанию, ресурс настроен, `device.client`, роль `Primary` | `PlanError::ConvertWhilePrimary` (проверяется раньше `UninitialisedFileWithLiveResource`) |
| узел с копией по описанию, ресурс настроен, `device.client`, роль не `Primary` | `Down`, затем действия как для ненастроенного ресурса по состоянию файла: `CreateFile`, `AttachLoop`, `LinkDevice`, `WriteRes`, `CreateMd`, `MarkReady`, `Up`, `MarkApplied` (каждое — только если нужно, по прежним строкам) |
| узел с копией, ресурс настроен, не `client`, файл отсутствует или без маркера | прежняя ошибка `UninitialisedFileWithLiveResource` (строка не меняется) |

Тесты: по одному на каждую строку; смена роли при файле `Ready` (маркер есть, ресурс `client`) — планируется `Down`, `AttachLoop`, …, `Up` без `CreateMd` (`create-md` только без маркера); копия с отсоединённым диском (`Diskless`, не `client`) и файлом без маркера — `UninitialisedFileWithLiveResource`, без `Down`. Запустить, увидеть падение.

- [ ] **Step 3: тесты исполнителя.** На фейковом `Runner` (как в существующих тестах `Reconciler`): `Down` вызывает `drbdadm down <том>`, перечитывает статус, при оставшемся ресурсе — `NotApplied`, следующие действия не выполняются; после `Down` `CreateMd` идёт через `dump-md` (правило фазы 2а не обходится). Мутации: без проверки роли падает тест `ConvertWhilePrimary`; с `disk_state == Diskless` вместо `client` падает тест копии с отсоединённым диском.

- [ ] **Step 4: реализация.** `MAX_ROUNDS` не меняется: смена роли укладывается в один раунд (`Down` и затем строки ненастроенного ресурса в том же списке). Шапку модуля дополнить строками таблицы и фразой: правило «`create-md` под настроенным ресурсом не выполняется» при смене роли соблюдается через `Down`.

- [ ] **Step 5: гейт (macOS и Linux) и коммит.**

```bash
git commit -m "feat(reconcile): turn a diskless node into a copy" -- src/reconcile.rs src/tests/reconcile.rs
```

---

### Task 5: `decide` — строки `Retiring`

**Files:**
- Modify: `src/agent/decide.rs`
- Modify: `src/agent/run.rs` (временная ветка в `reconcile_one`, задача 6 её заменяет)
- Test: `src/agent/tests/decide.rs`

**Interfaces:**
- Consumes: `NodeKind::Retiring`.
- Produces: `Task::RetireReplica` (снять копию узла по R26), `Task::FinishRetire` (на узле ничего нет: убрать запись статуса, финализатор, затем свою запись `spec.nodes`), `Refusal::RetiringWithoutFinalizer` («the node is Retiring but its finalizer is missing, the local copy is kept»), `Refusal::ReplicaWithoutFinalizer` («this node is a Replica without its finalizer; no copy is created»).

- [ ] **Step 1: тесты.** Строки (дописать в таблицу шапки модуля; первая подходящая строка побеждает; строка «узел указан дважды» по-прежнему первая):

| Объект | Узел | Финализатор узла | Локально | Задача |
|---|---|---|---|---|
| удаляется | `Retiring` | любой | любое | как у `Replica` в удаляемом объекте (те же строки) |
| есть | `Retiring` | есть | файл или следы | `RetireReplica` |
| есть | `Retiring` | есть или нет | ничего | `FinishRetire` |
| есть | `Retiring` | нет | файл или следы | `Refuse(RetiringWithoutFinalizer)` |
| есть | `Replica` | нет | нет файла | `Refuse(ReplicaWithoutFinalizer)` |
| есть | `Replica` | нет | файл | `Apply` (как сейчас: существующую копию не трогаем) |

Строка «`Retiring`, нет финализатора, только следы» — отказ, а не `CleanDiskless`: по R26 финализатор снимается после удаления копии, поэтому следы без финализатора — состояние вне порядка R26, а `CleanDiskless` выполнил бы `down` и отсоединение loop-устройств без проверок R26 (удалённый файл может держать loop-устройство). Тест на каждую строку; тест, что `Retiring` с совпадением `minor`/`port` получает `RetireReplica`, а не `Refuse(Collision)` (снятие не зависит от `collides_with`); мутация: без проверки финализатора в строке `RetireReplica` падает тест `RetiringWithoutFinalizer`. Запустить, увидеть падение.

- [ ] **Step 2: реализация.** Правило безопасности в шапке модуля: `Task::RetireReplica` возвращается только для существующего неудаляемого объекта, где узел `Retiring` и финализатор узла на объекте. В `reconcile_one` (`src/agent/run.rs`, `match` без `_`) — временные ветки: `RetireReplica` и `FinishRetire` пишут в журнал «not implemented yet», ничего не делают и возвращают `Outcome::Kept`. Задача 6 заменяет их; между задачами 5 и 6 образ на стенд не ставится.

- [ ] **Step 3: гейт и коммит.**

```bash
git commit -m "feat(agent): decide retiring copies" -- src/agent/decide.rs src/agent/tests/decide.rs src/agent/run.rs
```

---

### Task 6: исполнение снятия копии в цикле согласования

**Files:**
- Modify: `src/agent/run.rs` (`reconcile_one`, новые `retire_replica`, `finish_retire`, `retire_reasons`, шапка модуля)
- Modify: `src/agent/kube.rs` только если не хватает существующих вызовов (ожидается, что хватает: `replace_members`, `remove_finalizer`, `clear_entry` через `Agent`)
- Test: `src/agent/tests/run.rs`, фейки в `src/tests/volumes.rs`

**Interfaces:**
- Consumes: `Task::RetireReplica`, `Task::FinishRetire` (задача 5); `Reconciler::apply`, `observe`, `remove_checked`, `remove_unconfigured`, `status_matches_spec`; `bring_up`, `prove_no_data`, `clear_entry`, `drop_finalizer` (фаза 2а); протоколы P11 и P12.
- Produces: исходы `Outcome::Progress` (ресурс приведён к описанию, копия снята или шаг завершения выполнен), `Outcome::KeptDrbd` (причина — состояние DRBD: этот узел `Primary`, сторона не соединена, `peer-disk` копии не `UpToDate`), `Outcome::Kept` (остальное).

- [ ] **Step 1: тесты причин.** Чистая функция `fn retire_reasons(spec: &ReplicatedVolumeSpec, node: &str, status: &ResourceStatus) -> Vec<String>` (по образцу `keep_reasons`): пусто, если R26 (2)–(4) выполнены. Тесты: этот узел `Primary` — причина; сторона `Witness` в `Connecting` — причина; `Replica` с `peer-disk` `Inconsistent` — причина; сторона `Primary`, `Connected`, `UpToDate` — причины нет (отличие от удаления тома); запись `Client`, соединённая, без диска — причины нет; участник `spec.nodes` без соединения в статусе — причина. Мутации по каждому из трёх условий.

- [ ] **Step 2: тесты исполнения.** На фейках `Volumes` и `Runner`:
  1. Ресурс настроен и совпадает с описанием, причин нет: guard под `flock` повторно читает статус и причины; `remove_checked` выполнен; затем `apply_node_status(None)` и проверка (через `clear_entry`); затем `remove_finalizer`; затем `replace_members` со списком без этого узла и `expected` равным прочитанному. Порядок вызовов фиксируется тестом.
  2. Ресурс настроен, в `spec.nodes` есть участник, которого нет в файле ресурса или в локальном статусе (новый свидетель, новый `Client`): выполняется `Reconciler::apply` с описанием (`Retiring` с диском, `initialSync: Done`), исход `Progress`, ничего не удалено, `retire_reasons` не вызывается. Мутация: без этой ветки тест падает (проход уходит в причины «не соединён» навсегда).
  3. Ресурс настроен и совпадает, причины есть — ничего не удаляется, `message` записи узла содержит причину, исход `KeptDrbd`.
  4. Причины появились между проверкой и guard — копия остаётся.
  5. Ресурс не настроен, маркер `.img.ready` есть — `bring_up` (как в удалении тома), исход `Progress`, ничего не удалено.
  6. Ресурс не настроен, файла нет и loop-устройства нет — удаление через `remove_unconfigured` без проверок сторон, затем шаги завершения.
  7. `replace_members` вернул `false` (список изменился) — исход `Progress`; следующий проход (`FinishRetire`, финализатор уже снят) читает объект заново.
  8. `FinishRetire`: запись статуса есть — удаляется; финализатор есть — снимается; своя запись в `spec.nodes` — удаляется последней; ничего локально не трогается (нет вызовов `Runner`).
  9. Удаление записи статуса не подтвердилось — `Kept`, финализатор и `spec.nodes` не трогаются.
  Запустить, увидеть падение.

- [ ] **Step 3: реализация.** `retire_replica` повторяет структуру `delete_replica`: те же строки для ненастроенного ресурса; для настроенного — сначала сравнение с описанием (`observe`: `res_matches`, `res_applied`, `status_matches_spec`) и при расхождении `apply`, затем причины R26. Общие части вынести в функции, а не копировать (`bring_up` и `prove_no_data` уже общие). Временные ветки задачи 5 заменяются. `settle`: исходы как в таблице повторов спеки (`Progress` 1 с, `KeptDrbd` 5 с, `Kept` 30 с). Таблицу путей к удалению в шапке модуля дополнить строками `retire_replica` (настроенный ресурс, R26) и `retire_replica`, «данных нет»; описать R26, приведение ресурса к описанию перед проверками и порядок завершения.

- [ ] **Step 4: гейт (macOS и Linux) и коммит.**

```bash
git commit -m "feat(agent): remove a Retiring copy and its membership" -- src/agent/run.rs src/agent/tests/run.rs src/tests/volumes.rs
```

---

### Task 7: `NodeStageVolume` на снимаемом узле

**Files:**
- Modify: `src/csi/node.rs` (шаг 5 и таблица кодов в шапке)
- Test: `src/csi/tests/node.rs`

**Interfaces:**
- Consumes: `NodeKind::Retiring`.

- [ ] **Step 1: тесты.** Узел `Retiring` в объекте: `NodeStageVolume` отвечает `UNAVAILABLE` с текстом «the copy on this node is being removed», `spec.nodes` не меняется, ресурс не трогается, членство не снимается (код повторяемый). `NodeUnstageVolume` на узле `Retiring`: `leave_as_client` возвращает `Kept(Retiring)`, запись остаётся (как у `Replica`). Запустить, увидеть падение.

- [ ] **Step 2: реализация.** Проверка в `join` перед возвратом `AlreadyMember`: вид `Retiring` — `Status::unavailable`. Строка в таблице шагов шапки модуля.

- [ ] **Step 3: гейт и коммит.**

```bash
git commit -m "feat(csi): refuse staging on a node whose copy is being removed" -- src/csi/node.rs src/csi/tests/node.rs
```

---

### Task 8: `replicas` — правила команды

Чистый модуль без API и ввода-вывода.

**Files:**
- Create: `src/replicas.rs`, `src/tests/replicas.rs`
- Modify: `src/lib.rs` (`pub mod replicas;`)

**Interfaces:**
- Consumes: `ReplicatedVolumeSpec`, `ReplicatedVolumeStatus`, `MemberNode`, `NodeKind` (`crd`); `ClusterNodes` (`config`); `placement::{existing_of, free_budgets, pick_witness, Existing}` (задача 3); `membership::replica_finalizer`.
- Produces:

```rust
/// What the object looks like to the command: spec, status, deletion, finalizers.
pub struct View<'a> {
    pub spec: &'a ReplicatedVolumeSpec,
    pub status: Option<&'a ReplicatedVolumeStatus>,
    pub deleting: bool,
    pub finalizers: &'a [String],
}

/// One edit of the object: the new `spec.nodes` (sorted by name) and the
/// finalizer to add, if any. Applied only while `spec.nodes` is still the
/// list it was computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub nodes: Vec<MemberNode>,
    pub add_finalizer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Apply this edit, then call the planner again.
    Edit(Edit),
    /// Wait for the agents, then call the planner again; the text says what for.
    Wait(String),
    /// The requested change is complete.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeError { /* one variant per refusal of R25, each with the facts in its message */ }

pub fn next_add(view: &View, node: &str, cluster: &ClusterNodes, existing: &[Existing]) -> Result<Next, ChangeError>;
/// `seen_member`: this run of the command has already seen `node` as a member
/// or its finalizer on the object (tells "done" from "never a member").
pub fn next_remove(view: &View, node: &str, seen_member: bool, cluster: &ClusterNodes, existing: &[Existing]) -> Result<Next, ChangeError>;
pub fn next_move(view: &View, from: &str, to: &str, cluster: &ClusterNodes, existing: &[Existing]) -> Result<Next, ChangeError>;
```

Каждый вызов смотрит на текущее состояние и возвращает следующий шаг; так повторный запуск команды продолжает с места остановки (R24). `existing` — все объекты кластера, включая этот (`list_fresh`, через `existing_of`). Правило свидетеля: N — число `Replica` (без `Retiring`); свидетель нужен при чётном N. «Убрать свидетеля» — удалить запись, а если свидетель в `status.nodes` `Primary`, перевести в `Client` (R27). «Назначить свидетеля» — `pick_witness` с `clients` = участники вида `Client` (переход `Client → Witness`), `exclude` = узлы аргументов команды и все `Replica`/`Retiring`; новый участник — только при месте в `maxPeers + 1`; нет кандидата — без свидетеля.

- [ ] **Step 1: тесты `next_add`.** Состояния и ожидаемый результат (W — свидетель):
  1. Объект удаляется / узла нет в `ambar-nodes` — `ChangeError` в любом состоянии. До первого патча (узел не `Replica`): `initialSync: Skip` / есть `Retiring` / у какой-то `Replica` нет записи статуса или она не `UpToDate` / у участника нет записи / в записи участника соединение с другим участником не `Connected` / в записи участника `peerDisk` копии не `UpToDate` — соответствующие `ChangeError`. Отдельный тест «выключенный узел»: его собственная запись показывает `UpToDate` и все соединения `Connected` (запись устарела), а записи двух других узлов показывают его `Connecting` — `ChangeError`, в тексте имя узла и состояние соединения.
  2. Узел вне тома, N=2 с W, бюджета хватает — `Edit`: узел добавлен как `Replica` с `nodeId` и адресом из `ambar-nodes`, W остаётся, `add_finalizer: Some("ambar.balookrd.github.io/replica-<узел>")`; список отсортирован по имени.
  3. Узел вне тома, N=3 без W (станет 4) — `Edit` с узлом и новым W (участник `Client` первым). Места в `maxPeers + 1` хватает на узел, но не на W — `Edit` без W. Нет места и для узла — `ChangeError::NoRoom`.
  4. Узел — W (N=2 → 3) — `Edit`: `Witness → Replica`, финализатор и новый W по R27 (R22: свидетель был до правки). Кандидата нет — `Edit` без W.
  5. Узел — `Client` в `Primary` — `ChangeError` «move the pod first»; `Client` в `Secondary` — `Edit` с `Client → Replica`.
  6. Бюджета не хватает — `ChangeError::NoCapacity` с числами (узлу, который не `Replica` и не `Retiring` этого объекта, нужно не меньше `sizeBytes` свободного бюджета из `free_budgets`).
  7. Финализатор узла уже на объекте — `add_finalizer: None`.
  8. Узел уже `Replica`, записи статуса нет или она не `UpToDate` — `Wait("node … is <disk>; waiting for UpToDate")`, даже если в объекте есть `Retiring` или другие копии не `UpToDate` (продолжение, R25).
  9. Узел уже `Replica`, `UpToDate`, N нечётно, W есть — `Edit` «убрать свидетеля» (W `Primary` — W становится `Client`), `add_finalizer: None`.
  10. Узел уже `Replica`, `UpToDate`, правило свидетеля выполнено — `Done`.
  Запустить, увидеть падение.

- [ ] **Step 2: тесты `next_remove`.**
  1. Узел не участник и его финализатора нет — `ChangeError::NotAMember` (команда не правит свидетеля по опечатке в имени узла).
  2. Узел `Replica`, N=2 — `ChangeError::TooFewReplicas`. Тест «выключенный узел» из шага 1 для `remove` другой копии — `ChangeError` до первого патча; тот же тест, когда снимается сам несоединённый узел, — условие соединений его не касается, решают остальные условия R25 (снятие затем ждёт агента этого узла).
  3. Узел `Replica` в `Primary` — `ChangeError` «move the pod first».
  4. Узел `Replica`, сам не `UpToDate`, остальные `UpToDate` — `Edit` (снять недосинхронизированную копию можно, R25).
  5. Узел `Replica`, N=3 без W (станет 2) — `Edit`: узел `Retiring`, новый W (не этот узел), `add_finalizer: None`. Кластер из трёх узлов (все три — `Replica`, кандидата нет) — `Edit`: узел `Retiring`, без W.
  6. Узел `Replica`, N=4 с W (станет 3) — `Edit`: узел `Retiring`, W остаётся.
  7. Узел `Retiring` — `Wait("waiting for node … to remove its copy")` и текст `message` его записи статуса, если она есть.
  8. Узел исчез из `spec.nodes`, финализатор ещё на объекте — `Wait`.
  9. Узел исчез, финализатора нет: первый вызов этой команды увидел его участником (параметр `seen_member: bool` передаётся вызывающим — см. ниже), N=3 и W есть — `Edit` «убрать свидетеля»; N=2 и W нет — `Edit` «назначить свидетеля», снятый узел среди кандидатов (кластер из трёх узлов: свидетелем становится снятый узел с `nodeId` и адресом из `ambar-nodes`); правило выполнено — `Done`.
  Флаг `seen_member` различает строки 1 и 9: при повторном запуске команды после завершения снятия первый вызов видит узел ушедшим с ложным флагом и получает `NotAMember`; лишний свидетель при нечётном N в этом случае остаётся (кворуму не мешает), отчёт команды говорит об этом в тексте ошибки.
  Запустить, увидеть падение.

- [ ] **Step 3: тесты `next_move`.** Чётность N до и после одна, свидетель не меняется, кроме случая, когда целевой узел — свидетель:
  1. `from == to` — `ChangeError`.
  2. Исходный узел не участник и его финализатора нет: целевой `Replica` и `UpToDate` — `Done` (повтор завершённого переноса); иначе `ChangeError::NotAMember`.
  3. N=2 с W, целевой вне тома — `Edit`: целевой `Replica` с финализатором, W остаётся (вторым патчем свидетель не убирается).
  4. Целевой — W — `Edit`: `Witness → Replica`, финализатор, новый W по R27 (исходный узел исключён из кандидатов).
  5. Целевой `Replica`, не `UpToDate` — `Wait`.
  6. Целевой `Replica`, `UpToDate`, исходный `Replica` — `Edit`: исходный `Retiring`, свидетель не меняется; условия `remove` (исходный не `Primary`, после снятия не меньше двух `Replica`) проверяются здесь, а не до первого патча.
  7. Исходный `Retiring` или ушёл, финализатор есть — `Wait`; ушёл и финализатора нет — строка 2. Шаги добавления в этих состояниях не проверяются (R24).
  8. До первого патча исходный узел `Primary` — `ChangeError` «move the pod first» сразу, без добавления копии.
  Запустить, увидеть падение.

- [ ] **Step 4: реализация.** Модульный комментарий перечисляет R22–R25, R27 и таблицы «состояние → следующий шаг» для трёх функций. Сортировка `nodes` по имени в каждом `Edit`.

- [ ] **Step 5: гейт и коммит.**

```bash
git commit -m "feat(replicas): pure rules of replica add, remove and move" -- src/replicas.rs src/tests/replicas.rs src/lib.rs
```

---

### Task 9: команда `ambar replica`

**Files:**
- Create: `src/agent/replica_cmd.rs`, `src/agent/tests/replica_cmd.rs`
- Modify: `src/agent/mod.rs`, `src/agent/kube.rs` (новый вызов `edit_members`), `src/main.rs`, `src/tests/cli.rs` (если там проверяется разбор аргументов)
- Test: `src/agent/tests/kube.rs`

**Interfaces:**
- Consumes: `replicas::{next_add, next_remove, next_move, Next, Edit, View}` (задача 8); `Volumes`, `KubeVolumes`, `ConfigMapNodes` (фаза 2а).
- Produces: в `Volumes` метод `async fn edit_members(&self, name: &str, uid: &str, expected: &[MemberNode], edit: &Edit) -> Result<bool, ApiError>` — JSON-патч: `test` `/spec/nodes` равен `expected`; `replace` `/spec/nodes`; если `edit.add_finalizer` есть и его нет в объекте — `test` `/metadata/finalizers` равен прочитанному списку и `add` в конец (или `add` `/metadata/finalizers` со списком из одного элемента, если поля нет). Повтор при конфликте — через `patch_retrying`, как у `replace_members`. `false` — объект изменился, команда читает его заново. Фейк в `src/tests/volumes.rs` получает метод с записью вызовов.

Команды:

| Команда | Что делает |
|---|---|
| `ambar replica add <том> <узел>` | цикл `next_add`: `Edit` — `edit_members` и строка `edit: <что изменено>` в выводе, `Wait` — пауза 5 с и печать текста (не чаще раза в 30 с при неизменном тексте), `Done` — печать `OK: <узел> holds a copy of <том>`, код 0 |
| `ambar replica remove <том> <узел>` | то же с `next_remove`; `seen_member` становится истинным, как только команда увидела узел участником или его финализатор на объекте; `Done` — `OK: <узел> holds no copy of <том>` |
| `ambar replica move <том> <с узла> <на узел>` | цикл `next_move`; `Done` — `OK: the copy of <том> moved from <с узла> to <на узел>` |

Флаги: `--namespace` (переменная `POD_NAMESPACE`, по умолчанию `ambar-system`, как у `agent`), `--timeout <секунды>` (по умолчанию 0 — без предела; при истечении — код 1 и текст «timed out; run the same command again to continue», объект не откатывается). Отказ `ChangeError` — код 1, текст ошибки. Ошибка API — повтор чтения через 5 с, после трёх подряд — код 1. На каждой итерации объект читается заново с API-сервера (`get`), `existing` — `list_fresh`, карта узлов — `ConfigMapNodes::load`. `SIGINT` прерывает ожидание, начатый патч доходит до конца.

- [ ] **Step 1: тесты `edit_members_ops`.** Чистая функция операций (по образцу `replace_members_ops`): список не совпал — `None`; без финализатора — две операции; с новым финализатором при существующем списке — четыре операции; при отсутствующем поле — `add` со списком; финализатор уже есть — две операции.

- [ ] **Step 2: тесты цикла.** На фейках `Volumes` и карты узлов, на приостановленном времени tokio: `add` на свободный узел — один `edit_members`, затем `Wait` пока фейк не выставит `UpToDate`, затем `Done`, код 0; `edit_members` вернул `false` — объект читается снова, патч пересчитывается; `ChangeError` — код 1 без патча; `--timeout` истёк в ожидании — код 1, патчей после истечения нет; `move` при чётном N делает ровно два `edit_members` (целевой `Replica`, затем исходный `Retiring`) и не меняет свидетеля; повторный `move` после прерывания на ожидании снятия продолжает ждать и не делает патчей; повторный `move` после завершения — код 0 без патчей.

- [ ] **Step 3: реализация.** Клиент Kubernetes — `kube::Client::try_default()` (внутри пода — сервисный аккаунт `ambar`; права `get`/`list`/`patch` на `replicatedvolumes` и `get` на `ConfigMap ambar-nodes` уже выданы в `deploy/rbac.yaml`, проверить по файлу и записать в отчёт). `main.rs`: подкоманда `Replica` с вложенными `Add`, `Remove`, `Move`; справка (`after_help`) с примером запуска через `kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica add <том> <узел>` и кодами выхода.

- [ ] **Step 4: гейт и коммит.**

```bash
git commit -m "feat(cli): ambar replica add, remove and move" -- src/agent/replica_cmd.rs src/agent/tests/replica_cmd.rs src/agent/mod.rs src/agent/kube.rs src/agent/tests/kube.rs src/tests/volumes.rs src/main.rs <src/tests/cli.rs, если менялся>
```

---

### Task 10: сквозная проверка на стенде

**Files:**
- Create: `stand/replica-e2e.sh`
- Modify: `stand/README.md`
- Modify: `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` (раздел «Проверки смены набора копий на стенде»)

**Interfaces:**
- Consumes: всё предыдущее; `stand/install.sh`, `stand/lib.sh` (`kc`, `tb`).

Скрипт по образцу `stand/csi-e2e.sh`: `set -euo pipefail`, `fail` печатает диагностику один раз, ловушка `EXIT`/`INT` удаляет созданные объекты сценария, пустой вывод неудачной команды не считается успехом. Команда запускается так: `kc -n ambar-system exec "$(pod_on <узел>)" -c ambar -- ambar replica …`, где `pod_on` выбирает под агента на узле.

- [ ] **Step 1: собрать и установить.**

Run: `cargo zigbuild --release --locked --target aarch64-unknown-linux-musl && image/build.sh && NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh`
Expected: `OK: ambar installed on 4 nodes`.

- [ ] **Step 2: сценарии скрипта.** Каждый шаг проверяется утверждением, не печатью:
  - a. PVC класса `ambar-r2` и под-писатель на `dv-1`: под пишет в файл по строке с номером и `fsync` каждую секунду (как `probe.sh`), номер последней подтверждённой строки пишется в лог пода. Объект: две `Replica`, один `Witness`.
  - b. `replica add <том> <узел-свидетель>`: код 0; объект — три `Replica`, свидетеля нет; финализатор нового узла есть; файл тома на узле есть; под не перезапускался; номера в логе писателя растут без пропусков дольше 5 с.
  - c. `replica remove <том> <узел с копией, не dv-1>`: код 0; две `Replica` и новый `Witness`; на снятом узле нет файла тома, файла ресурса, ссылки и ресурса в `drbdsetup status`; финализатора узла нет.
  - d. `replica remove <том> dv-1` (под там): код 1, текст о `Primary`; объект не изменился.
  - e. `replica move <том> <копия не на dv-1> <узел вне тома>` (узел, который не `Replica` и не свидетель; скрипт выбирает его по объекту): код 0; копии на `dv-1` и на целевом узле, свидетель тот же, что до переноса (N чётно до и после); в выводе команды ровно две строки `edit:`. Затем `replica move` на узел свидетеля: код 0, назначен другой свидетель, если в кластере есть кандидат.
  - f. Повтор: `replica move … --timeout 20` на 2-ГиБ томе (второй PVC, перед переносом записаны 1.5 ГиБ данных) завершается кодом 1 во время синхронизации; скрипт проверяет, что в контейнере агента не осталось процесса `ambar replica` (`pgrep -f 'ambar replica'` пусто). Затем тот же `move` без `--timeout` доводит до конца. Сумма строк `edit:` в выводе обоих запусков — 2.
  - g. Правила CRD: `kubectl patch --type=json` удаляет запись `Replica` напрямую — отказ с сообщением `a Replica node is removed only through Retiring`; меняет вид `Replica → Client` — `this change of a node's kind is not allowed`; добавляет запись `Retiring` — `a node can become Retiring only from Replica`; два `Retiring` — `at most one node can be Retiring`. Каждое сообщение проверяется `grep`.
  - h. Писатель останавливается; последняя подтверждённая строка есть в файле на томе (под читает файл после `sync`); `kubectl delete pvc` удаляет том, на всех узлах нет файлов и ресурсов тома.
  - i. `grep -ci forbidden` по журналам всех контейнеров четырёх подов агента — 0; число перезапусков подов агента не изменилось.

- [ ] **Step 3: прогон.** Дважды подряд:

Run: `NODES="dv-1 dv-2 dv-3 dv-4" stand/replica-e2e.sh`
Expected: последняя строка `OK: replica e2e`; затем `NODES="dv-1 dv-2 dv-3 dv-4" stand/csi-e2e.sh` — `OK: csi e2e`; `NODES=… stand/check.sh` — код 0.

Если шаг падает, исполнитель находит причину (журналы агентов, `kubectl get rv -o yaml`, `drbdsetup status --verbose` на узлах, `dmesg`), а не повторяет прогон до успеха; исправление кода — отдельный коммит с тестом, воспроизводящим причину.

- [ ] **Step 4: записать.** В протокол: хэш коммита, длительность каждого прогона, время синхронизации в шаге b и e, последние строки, число `forbidden`, перезапуски подов. `stand/README.md`: строка о скрипте и его длительности.

```bash
git commit -m "test(stand): replica set change e2e" -- stand/replica-e2e.sh stand/README.md docs/superpowers/specs/2026-11-12-drbd9-spike-results.md
```

---

### Task 11: документы

**Files:**
- Modify: `docs/superpowers/specs/2026-11-12-ambar-design.md`
- Modify: `README.md`, `README.ru.md`
- Modify: `docs/superpowers/plans/2026-11-14-ambar-phase2b-replica-set.md` (только отметки шагов, если исполнитель их ведёт)

- [ ] **Step 1: спека.** По фактам кода и протоколов:
  - таблица фаз: 2б исполнена со ссылкой на раздел протокола задачи 10;
  - «Принятые решения»: решения владельца 2026-11-14 и R19–R31 в том виде, в каком они реализованы (где ревью изменили решение — дата замены), строка о смене набора копий (сейчас «Реализуется отдельным планом фазы 2б») переписывается;
  - `ReplicatedVolume`: вид `Retiring` в таблице полей и в R1; таблица правил валидации — тринадцать правил с сообщениями;
  - R10 переписывается: копия удаляется в двух случаях — удаление объекта (R10, R18) и снятие копии `Retiring` (R19, R26); таблица путей к удалению дополняется;
  - новый раздел «Смена набора копий» в «Жизненный цикл тома»: команда, флаги, коды выхода, таблица «состояние → следующий шаг» из `replicas`, снятие копии агентом (R26, порядок завершения), смена роли на узле (R28), почему агент не выполняет `forget-peer` (R29, запись P13);
  - «Агент `ambar`», таблица команд: `ambar replica`;
  - «Чего драйвер после фазы 2а не делает»: строка «Добавление, удаление и перенос копии» убирается; добавляются ограничения 2б: перенос копии на узел с работающим подом не выполняется; слоты битовой карты не освобождаются (R29): тому хватает слотов, пока к нему подключалось не больше `maxPeers` разных `nodeId`, поведение при нехватке — по записи P13; повторный `CreateVolume` того же тома после смены числа копий отвечает `ALREADY_EXISTS` (сравнение числа `Replica` с параметром `StorageClass`); кворум при синхронизирующейся копии — по записи P14; узел, умерший навсегда, нельзя снять командой (`remove` ждёт агента этого узла) — выход ручной, как для удаления тома;
  - заголовок даты: «приведена к коду фазы 2б <дата>».

- [ ] **Step 2: README.** В обоих файлах: раздел о смене набора копий с примером команды и кодами выхода; число тестов (вывод `cargo test --locked`, сумма по всем бинарям и библиотеке).

- [ ] **Step 3: проверить стиль и чистоту.** поиск по изменённым файлам строк из списка владельца (адреса домашней сети, домашний домен, путь домашнего каталога; список даёт контроллер, в файлы репозитория он не пишется) — пусто; в русском тексте нет слов из запретного списка стиля (надёжный, эффективно, обеспечивает, гарантирует, просто, ключевой, удобный, гибкий).

```bash
git commit -m "docs: phase 2b replica set changes in spec and READMEs" -- docs/superpowers/specs/2026-11-12-ambar-design.md README.md README.ru.md
```

---

## Что остаётся после этого плана

| Что | Где |
|---|---|
| Перенос копии на узел, где работает под (смена роли у `Primary`) | не запланировано; до кода — опыт с `detach`/`attach` у `Primary` |
| Снятие копии с узла, умершего навсегда, командой | фаза 3 вместе с остальной обработкой мёртвого узла |
| Перебалансировка при добавлении и выводе узла из кластера | не запланировано |
| Чтение метаданных файла без маркера готовности на пути удаления | фаза 3 (решение владельца 2026-11-14) |
