# План реализации: Сессия 1 — Безопасность Kubernetes API и эвикция подов ([C-2], [H-3], [H-4])

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-СКИЛЛ: Используйте `superpowers:subagent-driven-development` или `superpowers:executing-plans` для пошаговой реализации плана. Шаги отслеживаются через чекбоксы (`- [ ]`).

**Цель:** Устранить критические и высокоприоритетные уязвимости ядра Kubernetes API в ambar: предотвратить ошибочное выселение чужих и системных подов при потере кворума, устранить TOCTOU при удалении подов, обеспечить атомарность JSON Patch для taints и finalizers, а также гарантировать повтор эвикции при временных сбоях API.

**Архитектура:**
1. В `src/agent/kube.rs`:
   - В `remove_taint_ops` добавить проверку `op: "test"` по `/spec/taints/{idx}/key` и `/spec/taints/{idx}/effect` перед `op: "remove"`.
   - В `ensure_taint_ops` и `edit_members_ops` при создании новых массивов (когда они были `None`) добавить `op: "test"` по `/metadata/resourceVersion`.
   - В `pod_uses_volume` валидировать имя тома через `valid_volume_name` и проверять исключительно `mounted_uids`. Убрать эвристики `v.name == volume` и `pvc.claim_name == volume`.
   - В `pod_uses_volume_resolve` валидировать имя тома, опрашивать PVC через API, строго сверять `pvc.spec.volume_name == Some(volume)` и пробрасывать ошибки API наружу (`Result<bool, ApiError>`) вместо проглатывания (fail-closed).
   - В `delete_node_pods_for_volume`: передавать `Preconditions { uid: pod.uid }`, пропускать mirror-поды (`kubernetes.io/config.mirror`), уже удаляемые поды (`deletion_timestamp.is_some()`) и завершённые поды (`Succeeded` / `Failed`), обрабатывать 404/409 без сбоя цикла.
   - В `has_node_pods_for_volume`: возвращать `Err` при ошибке `pod_uses_volume_resolve`.
2. В `src/agent/quorum.rs`:
   - Добавить метод `QuorumTracker::retry_eviction(volume)`.
   - В `check_volume_quorum`: при ошибке `delete_node_pods_for_volume` вызывать `retry_eviction`, чтобы на следующем тике эвикция повторилась, и логировать предупреждение.
3. В `src/tests/volumes.rs`:
   - В `FakeVolumes` добавить вспомогательную функцию для mock-тестов без реального API-сервера, сохранив обратную совместимость синтетических тестов `src/agent/tests/run.rs`.

**Стек технологий:** Rust (Edition 2024), `kube-rs`, `k8s-openapi`, `tokio`, `serde_json`.

## Глобальные ограничения
- Следовать TDD: сначала падающий тест, затем минимальная реализация, затем прогон тестов.
- Все существующие тесты (921 шт.) должны проходить.
- `cargo clippy --locked --all-targets -- -D warnings` должен быть чистым.
- Коммит в `main` после завершения сессии строго с указанием файлов: `git commit -m "..." -- <files>`.

---

### Задача 1: Атомарность JSON Patch для taints и finalizers ([H-3])

**Файлы:**
- Модифицировать: `src/agent/kube.rs:288-322, 458-470`
- Тесты: `src/agent/tests/kube.rs:215-260`

**Интерфейсы:**
- `ensure_taint_ops(node: &Node, key: &str, effect: &str) -> Option<Vec<Value>>`
- `remove_taint_ops(node: &Node, key: &str, effect: &str) -> Option<Vec<Value>>`
- `edit_members_ops(object: &ReplicatedVolume, expected: &[MemberNode], edit: &Edit) -> Option<Vec<Value>>`

- [x] **Шаг 1: Написать падающие тесты на атомарные `test`-операции**
- [x] **Шаг 2: Запустить тесты и убедиться в падении**
- [x] **Шаг 3: Реализовать атомарные проверки в `src/agent/kube.rs`**
- [x] **Шаг 4: Запустить тесты и убедиться в успешном прохождении**

---

### Задача 2: Гарантия повтора эвикции подов при сбоях API ([H-4])

**Файлы:**
- Модифицировать: `src/agent/quorum.rs:18-96`
- Модифицировать: `src/agent/run.rs:1630-1636`
- Тесты: `src/agent/tests/quorum.rs:175-195`

**Интерфейсы:**
- `QuorumTracker::retry_eviction(&mut self, volume: &str)`
- `check_volume_quorum(tracker, volumes, check) -> Result<Option<Vec<String>>, ApiError>`

- [x] **Шаг 1: Написать падающий тест на повтор эвикции после ошибки API**
- [x] **Шаг 2: Запустить тест и убедиться в падении**
- [x] **Шаг 3: Реализовать `retry_eviction` и логирование в `src/agent/quorum.rs` и `src/agent/run.rs`**
- [x] **Шаг 4: Запустить тесты и убедиться в успешном прохождении**

---

### Задача 3: Безопасность проверки подов, fail-closed и TOCTOU-защита эвикции ([C-2])

**Файлы:**
- Модифицировать: `src/agent/kube.rs:324-375, 740-785`
- Модифицировать: `src/tests/volumes.rs:520-565`
- Тесты: `src/agent/tests/kube.rs:280-340`

**Интерфейсы:**
- `pod_uses_volume(pod: &Pod, volume: &str, mounted_uids: &[String]) -> bool`
- `pod_uses_volume_resolve(client: &Client, pod: &Pod, volume: &str, mounted_uids: &[String]) -> Result<bool, ApiError>`
- `delete_node_pods_for_volume(&self, node: &str, volume: &str, mounted_uids: &[String]) -> Result<Vec<String>, ApiError>`
- `has_node_pods_for_volume(&self, node: &str, volume: &str, mounted_uids: &[String]) -> Result<bool, ApiError>`

- [x] **Шаг 1: Написать падающие тесты на безопасность `pod_uses_volume` и `delete_node_pods_for_volume`**
- [x] **Шаг 2: Реализовать безопасную логику в `src/agent/kube.rs` и `src/tests/volumes.rs`**
- [x] **Шаг 3: Запустить тесты и убедиться в успешном прохождении**

---

### Задача 4: Полная верификация и коммит

- [x] **Шаг 1: Запустить полный набор тестов**
- [x] **Шаг 2: Запустить проверки форматирования и линтер**
- [ ] **Шаг 3: Закоммитить изменения Сессии 1**
