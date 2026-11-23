# AGENTS.md

## Область действия

Корневой файл репозитория `ambar`. Действует на весь проект.

Память проекта и база знаний (топология, история инцидентов, стенды):
- `~/.claude/projects/<project-slug>/memory/` (главный индекс в `MEMORY.md`).
- Спецификации и отчеты в `docs/superpowers/`.

## Что это

`ambar` — Kubernetes CSI-драйвер для реплицируемых томов PVC поверх модуля ядра DRBD 9 на небольших кластерах k3s (целевое железо: FriendlyElec NanoPi R5C, Debian 13 trixie arm64, ядро 6.12+).
Проект написан на Rust (Edition 2024).

## Экономия токенов и контекста (Token Efficiency)

1. **Запретные для сканирования и чтения директории**:
   - `target/` — артефакты компиляции Rust (категорически запрещены для чтения).
   - `.git/`, `.idea/`, `.superpowers/`.
   - Lock-файлы (`Cargo.lock`) — не читать целиком; проверять зависимости через `Cargo.toml` или `cargo tree`.

2. **Карта кода и AST-индексация (`.code-index/` и `scripts/code-index.py`)**:
   - Для высокоуровневой навигации начинай с локальной карты `.code-index/repo-map.md` и `.code-index/symbols.json` (генератор `scripts/code-index.py` по умолчанию извлекает Rust-символы и `impl`-блоки из AST через `ast-grep` и сохраняет диапазон `line`..`end_line`).
   - После заметных структурных изменений в `src/` (добавление или переименование модулей, структур, публичных функций) обновляй индекс командой `python3 scripts/code-index.py`. Подробности и примеры запросов — в `docs/CODE-INDEX.ru.md`.

3. **Приоритет AST-поиска и AST-рефакторинга (`ast-grep`)**:
   - В проекте подключён **`ast-grep`** (CLI `ast-grep`, конфиг `sgconfig.yml`, MCP-сервер `ast-grep` в `.mcp.json`).
   - По возможности для любого структурного поиска по Rust-коду (объявления функций, структур, enum, трейтов, `impl`-блоков, вызовы, макросы, match-ветки) используй AST вместо текстового `grep`/`rg`:
     - через MCP: `find_code` (паттерн), `find_code_by_rule` (YAML-правило с `kind`/`has`/`inside`, особенно когда модификаторы `pub`/`async`/`unsafe` могут отличаться), `dump_syntax_tree` и `test_match_code_rule` (отладка правил);
     - через CLI: `ast-grep run -l rust -p '...' src/` или `ast-grep scan --inline-rules '...' src/`.
   - Для массовых однотипных структурных правок в Rust по возможности используй `rewrite_code` (MCP) или `ast-grep run -l rust -p '...' -r '...'` вместо `sed`/regex-замен.
   - Текстовый поиск (`rg`/`grep`) используй для не-кодовых файлов (YAML, Markdown, shell-скрипты, комментарии, строковые литералы) или когда AST-запрос невыразим.
   - Примечания: код возврата `1` у `ast-grep run` / `find_code` означает отсутствие совпадений (не сбой); вызывай бинарник по полному имени `ast-grep`, а не `sg` (на Linux `/usr/bin/sg` — системная утилита `shadow-utils`).

4. **Чтение файлов только фрагментами (Slice Viewing)**:
   - Не читать файлы длиннее 100 строк целиком; используй диапазоны `line`..`end_line` из `.code-index/symbols.json` и координаты совпадений `ast-grep`, открывая только релевантные строки (`StartLine`/`EndLine`).

5. **Делегирование тяжелых поисков субагентам**:
   - При масштабных исследованиях запускать субагента `research` для сохранения контекста основной сессии.

## Ключевые архитектурные инварианты

1. **Сопоставление Pod Volume и PV в CSI**:
   - В объекте `Pod.spec.volumes[i].persistent_volume_claim.claim_name` указано имя PVC, тогда как имя ReplicatedVolume в ambar формируется по имени PV (`pvc-<UUID>`).
   - При проверке принадлежности тома поду обязательно запрашивать API PVC для сверки `pvc.spec.volume_name == volume`, иначе агент ошибочно считает, что подов на узле нет, и преждевременно удаляет `Client`.

2. **CEL Budget в CRD (Kubernetes 1.30+)**:
   - Строковые конкатенации (`.map(n, n.name + '/' + n.kind)`) и аллокации динамических списков в CEL-правилах превышают лимит стоимости валидатора Kubernetes (`cost limit`).
   - Все правила валидации переходов множеств узлов должны формулироваться через предикаты `.all(...)` и `.exists(...)` без строковых конкатенаций.

3. **DRBD 9: Синтаксис drbdadm и конфликты minor**:
   - `drbdadm` при любом вызове (включая `down`) парсит ВСЕ файлы в `/etc/drbd.d/*.res`.
   - Если на диске остался старый `.res` файл с тем же `minor`, `drbdadm` завершается с кодом 10 (`conflicting use of device-minor`), блокируя любые операции.
   - Перед вызовом `drbdadm down` при очистке клиентов необходимо под файловым локом тома проверять статус DRBD: если устройство открыто (`open: true`) или Primary, вызывать `down` нельзя во избежание D-state в ядре.

4. **Локальный стенд Lima**:
   - Запуск стенда: `NODES="dv-1 dv-2 dv-3 dv-4" stand/up.sh && stand/k3s.sh`
   - Установка ambar: `NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh`
   - Тест отказов: `NODES="dv-1 dv-2 dv-3 dv-4" stand/failure-e2e.sh`
   - Очистка стенда: `NODES="dv-1 dv-2 dv-3 dv-4" stand/down.sh`

5. **Аллокатор памяти (`jemalloc`)**:
   - Фича `jemalloc` включена по умолчанию (`tikv-jemallocator` + `tikv-jemalloc-ctl`) с `MALLOC_CONF` (`background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:0,calloc_madvise_threshold:0`).
   - Причина: `cargo zigbuild` (Zig 0.17.1) подменяет `musl mallocng` на `SmpAllocator` (`max_alloc_search = 1`, без возврата слабов `< 64 KiB`), что вызывало утечку ~800 МиБ/сутки из-за кросс-поточного `alloc → free` в `SystemRunner` и `tick_failures`.
   - Страница jemalloc для `aarch64-unknown-linux-musl` / `gnu` закреплена на 4 КиБ (`LG_PAGE=12` в `.cargo/config.toml`), проверяется при старте агента и тестом `jemalloc_page_matches_the_kernel_page`.

## Рабочий процесс

- Спецификации и планы (`docs/superpowers/**`) пишутся на русском языке.
- Двуязычная документация (`README.md`/`README.ru.md`, `CHANGELOG.md`/`CHANGELOG.ru.md`, `docs/*.md`/`docs/*.ru.md`) обновляется синхронно в обеих версиях.
- Релизы ведутся по CalVer (`vYY.M.PATCH`): подготовка коммита `release: cut v...` через ручной workflow `.github/workflows/release.yml`, публикация бинарников `musl` и release notes (`git-cliff`) через `.github/workflows/tag-release.yml` по локальному подписанному тегу `git tag -s`.
- Перед коммитом проверять форматирование и линтер:
  ```bash
  cargo fmt --check
  cargo clippy --all-targets --no-deps -- -D warnings
  cargo test
  ```
