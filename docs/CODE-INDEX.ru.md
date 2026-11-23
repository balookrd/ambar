# Индекс кода

`scripts/code-index.py` строит локальный структурный индекс Rust-кодовой базы для
навигации с малым расходом токенов. Он записывает Cargo-пакеты, target'ы,
исходные файлы, Rust items (`fn`, `struct`, `enum`, `trait`, `union`, `type`,
`const`, `static`, `mod`, `macro_rules`) и заголовки `impl`.

По умолчанию (`--parser auto`) скрипт извлекает объявления из Rust AST через
**`ast-grep`** (`ast-grep scan --inline-rules`) и автоматически откатывается на
встроенный regex-парсер, если `ast-grep` отсутствует в `PATH`. Для работы нужны
только `python3` и `cargo` в `PATH` (список пакетов берётся из
`cargo metadata --no-deps`); дополнительных Python-пакетов не требуется.

Индекс — карта, а не замена исходникам: по нему найдите нужный модуль, символ и
диапазон строк `line`..`end_line`, а затем откройте только те фрагменты
исходного кода, которые нужны для правки.

## Генерация

Запускайте из корня репозитория:

```bash
python3 scripts/code-index.py
```

Результаты:

- `.code-index/repo-map.md` — компактная Markdown-карта для человека и
  агента.
- `.code-index/symbols.json` — машинно-читаемые данные по пакетам, файлам
  и символам.

`.code-index/` игнорируется git'ом, поэтому сгенерированный индекс остаётся
локальным и не сбрасывается при `cargo clean`.

## Параметры

| Флаг | По умолчанию | Что делает |
|---|---|---|
| `--package <name>` | все пакеты | ограничить индекс одним пакетом; для нескольких повторить флаг |
| `--parser <mode>` | `auto` | выбор парсера: `auto` (`ast-grep` с fallback на `regex`), `ast-grep` (требовать AST) или `regex` |
| `--self-test` | выключен | прогнать встроенный самотест парсеров во временном каталоге без записи результата |
| `--output-dir <dir>` | `.code-index` | куда писать `repo-map.md` и `symbols.json`; относительный путь считается от `--repo-root` |
| `--repo-root <dir>` | `.` (текущий каталог) | корень репозитория |

## Примеры запросов

Найти, где объявлен символ:

```bash
rg 'should_cleanup_client|TaintTracker|StandaloneTracker' .code-index/repo-map.md
```

Посмотреть JSON через `jq` (включая диапазоны `line`..`end_line` для точного чтения фрагментов):

```bash
jq '.packages[] | select(.name == "ambar") | .source_files[] | select(.path | contains("agent/")) | {path, module, symbols: [.symbols[] | {name, line, end_line}]}' .code-index/symbols.json
```

Найти точный диапазон строк конкретного символа:

```bash
jq '.packages[] | .source_files[] | .path as $path | .symbols[] | select(.name == "TaintTracker") | {path: $path, kind, name, line, end_line}' .code-index/symbols.json
```

На верхнем уровне `symbols.json` записывается поле `parser` (`"ast-grep"` или
`"regex"`). У каждого символа есть поля `kind`, `name`, `line`, `end_line`,
`visibility`, `module` и `signature`, а у каждого блока `impl` — `line`,
`end_line`, `header` и `module`. При использовании `ast-grep` поле `end_line`
содержит последнюю строку объявления или `impl`-блока; в режиме `regex`
значение равно `null`.

## AST-поиск и рефакторинг (`ast-grep`)

В репозитории настроен `ast-grep` через [`sgconfig.yml`](../sgconfig.yml)
(`.ast-grep/{rules,tests,utils}`), а в [`.mcp.json`](../.mcp.json) подключён
MCP-сервер `ast-grep-mcp`, чтобы агенты могли выполнять структурные запросы
(`find_code`, `find_code_by_rule`, `rewrite_code`, `dump_syntax_tree`,
`test_match_code_rule`).

Примеры CLI-команд для `ambar`:

```bash
# Найти структуры по AST-шаблону
ast-grep run -l rust -p 'pub struct $NAME' src/

# Найти объявление независимо от модификаторов pub / pub(crate) / async
ast-grep scan --inline-rules '
id: find-taint-tracker
language: rust
rule:
  kind: struct_item
  has:
    field: name
    regex: ^TaintTracker$
' src/

# Пробная структурная замена (без -U показывает diff; -U применяет правки)
ast-grep run -l rust -p '$OLD_EXPR' -r '$NEW_EXPR' src/
```

Примечания:

- Вызывайте бинарник строго как `ast-grep` (не `sg`: на Linux `/usr/bin/sg` —
  это утилита из пакета `shadow-utils`).
- `ast-grep run` возвращает код выхода `1`, если совпадений нет, тогда как
  `ast-grep scan --json=compact` при нуле совпадений возвращает `0` и `[]`.
- Для типов, резолва имён и диагностики компилятора используйте
  `rust-analyzer`: ни индекс кода, ни `ast-grep` не резолвят типы между
  крейтами.
