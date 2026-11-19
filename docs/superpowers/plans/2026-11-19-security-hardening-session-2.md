# План реализации: Сессия 2 — Устранение гонок и fail-open в фоновых контурах ([H-5], [H-6], [H-9])

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-СКИЛЛ: Используйте `superpowers:subagent-driven-development` или `superpowers:executing-plans` для пошаговой реализации плана. Шаги отслеживаются через чекбоксы (`- [ ]`).

**Цель:** Устранить опасные политики fail-open и гонки без блокировок в контуре сбоев (failure loop), обеспечить изоляцию вызовов DRBD под `VolumeLocks` и файловым локом, а также устранить дефекты конечного автомата топологии реплик (`ConvertWhilePrimary` и потеря `Witness`).

**Архитектура:**
1. **[H-6] Fail-closed в `tick_failures` (`src/agent/run.rs`):**
   - При ошибке `read_all_statuses().await` прерывать тик с `Err(e)`: не снимать taint `no-quorum` и не перезаписывать метрики нулями при сбое опроса DRBD.
2. **[H-5] Синхронизация и блокировки в фоновом контуре (`src/agent/cleanup.rs`, `src/agent/run.rs`, `src/agent/standalone.rs`):**
   - В `check_and_cleanup_client`: брать `VolumeLocks` перед проверкой статуса и подов; при ошибке чтения статуса DRBD fail-closed (не сносить клиента при `Err`); в guard `run.rs:1471` возвращать `Err` при ошибке чтения DRBD.
   - В `handle_standalone`: перенести вызов `drbd.adjust` в `self.blocking(...)` под `VolumeLocks` и файловым локом тома; фильтровать только валидные тома ambar (`valid_volume_name`); проверять `out.success()` и логировать предупреждения при неудачах.
3. **[H-9] FSM смены топологии реплик (`src/replicas.rs`):**
   - В `next_add`: проверять `ConvertWhilePrimary` для `Client` и `Witness`.
   - В `next_move`: проверять `view.deleting`, требовать `from.kind == NodeKind::Replica`, проверять `ConvertWhilePrimary` для `to` (`Client` и `Witness`), вызывать `check_r25` перед переводом `from` в `Retiring`, и назначать освободившийся `from` свидетелем (`Witness`) при переходе на 2 реплики в 3-узловом кластере.

**Стек технологий:** Rust (Edition 2024), `tokio`, DRBD 9.

## Глобальные ограничения
- TDD: падающий тест -> минимальная реализация -> проверка.
- Все существующие тесты (925 шт.) должны проходить.
- `cargo clippy --locked --all-targets -- -D warnings` — 0 предупреждений.
- `cargo fmt --check` — без расхождений.
- Коммит в `main` после завершения сессии строго с указанием файлов: `git commit -m "..." -- <files>`.

---

### Задача 1: Fail-closed обработка статусов DRBD в `tick_failures` ([H-6])

**Файлы:**
- Модифицировать: `src/agent/run.rs:1604-1620`
- Тесты: `src/agent/tests/run.rs`

- [ ] **Шаг 1: Написать падающий тест на сохранение taint и метрик при сбое `read_all_statuses`**
  В `src/agent/tests/run.rs`:
  - Создать тест `tick_failures_fails_closed_when_read_all_statuses_errors`: если `drbdsetup status --json` завершается с ошибкой, узел с существующим taint `no-quorum` сохраняет taint, метрики не затираются пустыми данными, а метод возвращает `Err`.

- [ ] **Шаг 2: Запустить тест и убедиться в падении**
  ```bash
  cargo test --lib agent::run::tests::tick_failures_fails_closed
  ```

- [ ] **Шаг 3: Реализовать fail-closed в `src/agent/run.rs`**
  - Заменить `.unwrap_or_default()` на `?` (с логированием предупреждения): при ошибке немедленно возвращать `Err(e)` до снятия taints и обновления метрик.

- [ ] **Шаг 4: Запустить тесты и убедиться в успешном прохождении**
  ```bash
  cargo test --lib agent::run::tests::tick_failures
  ```

---

### Задача 2: Блокировки `VolumeLocks`, файловый лок и fail-closed в cleanup и standalone ([H-5])

**Файлы:**
- Модифицировать: `src/agent/cleanup.rs:52-83`
- Модифицировать: `src/agent/run.rs:1470-1481, 1642-1648`
- Модифицировать: `src/agent/standalone.rs:109-123`
- Тесты: `src/agent/tests/cleanup.rs`, `src/agent/tests/standalone.rs`, `src/agent/tests/run.rs`

- [ ] **Шаг 1: Написать падающие тесты на блокировки и fail-closed в cleanup и standalone**
  - В `src/agent/tests/cleanup.rs`: тест на то, что ошибка чтения DRBD-статуса (`Err`) в `check_and_cleanup_client` возвращает `Err` / не выполняет удаление клиента (fail-closed).
  - В `src/agent/tests/standalone.rs`: тест на то, что `handle_standalone` отклоняет невалидные имена томов.

- [ ] **Шаг 2: Запустить тесты и убедиться в падении**
  ```bash
  cargo test --lib agent::cleanup::tests
  cargo test --lib agent::standalone::tests
  ```

- [ ] **Шаг 3: Реализовать безопасные блокировки и fail-closed**
  - В `check_and_cleanup_client`: брать `let _lock = agent.locks.lock(volume).await;`.
  - При ошибке `agent.read_status(volume).await`: если `Err`, возвращать `Err(e)` (fail-closed), а не проглатывать в `None`.
  - В `cleanup_stale_client` (`run.rs:1471`): если `status` вернул `Err`, возвращать `Err("cannot read drbd status")`.
  - В `tick_failures` (секция standalone): запускать `handle_standalone` под `self.locks.lock(&status.name).await` через `self.blocking` с файловым локом тома под flock.

- [ ] **Шаг 4: Запустить тесты и убедиться в прохождении**
  ```bash
  cargo test --lib agent::cleanup::tests
  cargo test --lib agent::standalone::tests
  cargo test --lib agent::run::tests::tick_failures
  ```

---

### Задача 3: Исправление дефектов FSM топологии реплик ([H-9])

**Файлы:**
- Модифицировать: `src/replicas.rs:317-327, 477-615`
- Тесты: `src/replicas/tests.rs` (или `src/tests/replicas.rs`)

- [ ] **Шаг 1: Написать падающие тесты на FSM edge-cases**
  - Тест: `next_add` возвращает `ConvertWhilePrimary`, если узел-цель является `Witness` и находится в роли `Primary`.
  - Тест: `next_move` возвращает `ConvertWhilePrimary`, если узел-назначение (`to`) находится в роли `Primary`.
  - Тест: `next_move` возвращает ошибку, если `view.deleting == true` или `from.kind != Replica`.
  - Тест: `next_move` в 3-узловом кластере при переносе реплики на узел свидетеля назначает освободившийся узел `from` новым `Witness` перед `Done`.

- [ ] **Шаг 2: Запустить тесты и убедиться в падении**
  ```bash
  cargo test --lib replicas::tests
  ```

- [ ] **Шаг 3: Реализовать исправления в `src/replicas.rs`**
  - В `next_add`: добавить `|| n.kind == NodeKind::Witness` в проверку `ConvertWhilePrimary`.
  - В `next_move`:
    - Проверить `view.deleting` в начале.
    - Проверить `from_node.kind == NodeKind::Replica`.
    - Проверить `ConvertWhilePrimary` для `to` (`Client` и `Witness`).
    - Проверить `check_r25` перед выставлением `Retiring`.
    - В конце `next_move`, если активных реплик стало 2 в 3-узловом кластере, назначить освободившийся `from` как `Witness`.

- [ ] **Шаг 4: Запустить тесты и убедиться в прохождении**
  ```bash
  cargo test --lib replicas::tests
  ```

---

### Задача 4: Полная верификация и коммит Сессии 2

- [ ] **Шаг 1: Полный прогон `cargo test --locked`**
- [ ] **Шаг 2: Проверка `cargo fmt --check` и `cargo clippy --locked --all-targets -- -D warnings`**
- [ ] **Шаг 3: Коммит изменений Сессии 2**
  ```bash
  git commit -m "fix(agent): eliminate failure loop fail-open, add volume locks, and fix replica fsm (H-5, H-6, H-9)" -- src/agent/run.rs src/agent/cleanup.rs src/agent/standalone.rs src/replicas.rs src/agent/tests/run.rs src/agent/tests/cleanup.rs src/agent/tests/standalone.rs src/replicas/tests.rs docs/superpowers/plans/2026-11-19-security-hardening-session-2.md
  ```
