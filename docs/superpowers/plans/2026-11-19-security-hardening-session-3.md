# План реализации: Сессия 3 — Защита периметра и устойчивость демона (H-7, M-1, M-2)

План закрывает замечания архитектурного аудита:
- `[H-7]`: DoS и утечка файловых дескрипторов в Metrics HTTP-сервере, изоляция падения от завершения агента.
- `[M-1]`: Строгий роутинг в Metrics HTTP-сервере (поддержка query-параметров, валидация заголовков).
- `[M-2]`: Блокировки VolumeLocks, detached-вызовы и защита от path traversal (`..`) в CSI Node Service.

---

### Задача 1: Защита и роутинг Metrics HTTP-сервера ([H-7], [M-1])
1. В `src/agent/metrics_server.rs`:
   - Ввести константу `const MAX_CONCURRENT_METRICS_CONNECTIONS: usize = 32`.
   - Создать `Arc<tokio::sync::Semaphore>::new(MAX_CONCURRENT_METRICS_CONNECTIONS)`.
   - В цикле `accept()` использовать `semaphore.clone().try_acquire_owned()`. При отказе — немедленный ответ `503 Service Unavailable` и закрытие TCP-сокета.
   - В `handle_connection`:
     - Парсить путь запроса без query-параметров: `raw_path.split('?').next().unwrap_or(raw_path)`.
     - Защита от переполнения: если запрос без CRLF доходит до лимита буфера — возвращать `400 Bad Request`.
2. В `src/main.rs`:
   - Изолировать ошибку metrics-сервера от вызова `shutdown.cancel()`. Сбой HTTP-эндпоинта метрик не должен завершать процесс агента с кодом `0`.
3. Добавить тесты в `src/agent/tests/metrics_server.rs`:
   - `get_metrics_with_query_params_returns_200`.
   - `metrics_server_rejects_connections_exceeding_concurrency_limit`.
   - `malformed_request_returns_400`.

---

### Задача 2: CSI Node Service — VolumeLocks, detached и Path Traversal ([M-2])
1. В `src/csi/node.rs`:
   - В функции `absolute(field: &str, value: &str)`:
     - Проверять отсутствие `Component::ParentDir` (`..`).
   - В `node_publish_volume`:
     - Обернуть в `detached(...)`.
     - Захватывать `self.agent.locks.lock(&volume).await`.
     - `std::fs::create_dir_all` вызывать через `tokio::task::spawn_blocking`.
   - В `node_unpublish_volume`:
     - Обернуть в `detached(...)`.
     - Захватывать `self.agent.locks.lock(&volume).await`.
     - `std::fs::remove_dir` вызывать через `tokio::task::spawn_blocking`.
2. Добавить тесты в `src/csi/tests/node.rs`:
   - Проверка отклонения путей с `..` в `staging_target_path` и `target_path`.
   - Проверка корректности `publish` и `unpublish` под блокировкой `VolumeLocks`.

---

### Задача 3: Верификация и коммит
1. Запуск unit-тестов: `cargo test --locked`.
2. Проверка форматирования и линтера: `cargo fmt --check` и `cargo clippy --all-targets --no-deps -- -D warnings`.
3. Коммит изменений Сессии 3 с явным указанием файлов.
