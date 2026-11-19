# Индекс кода

`scripts/code-index.py` строит локальный структурный индекс Rust-кодовой базы для
навигации с малым расходом токенов. Он записывает Cargo-пакеты, target'ы,
исходные файлы, верхнеуровневые Rust items и заголовки `impl`. Нужны `python3` и
`cargo` в `PATH` (список пакетов берётся из `cargo metadata --no-deps`);
дополнительных Python-пакетов не нужно.

Индекс — карта, а не замена исходникам: по нему найдите нужный модуль и символ,
а затем откройте только те диапазоны исходного кода, которые нужны для правки.

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
| `--package <name>` | все пакеты | ограничить индекс одним пакетом |
| `--output-dir <dir>` | `.code-index` | куда писать `repo-map.md` и `symbols.json`; относительный путь считается от `--repo-root` |
| `--repo-root <dir>` | `.` (текущий каталог) | корень репозитория |

## Примеры запросов

Найти, где объявлен символ:

```bash
rg 'should_cleanup_client|VolumeQuorumCheck|StandaloneTracker' .code-index/repo-map.md
```

Посмотреть JSON через `jq`:

```bash
jq '.packages[] | select(.name == "ambar") | .source_files[] | select(.path | contains("agent/")) | {path, module, symbols: [.symbols[].name]}' .code-index/symbols.json
```

У каждого символа в `symbols.json` есть поля `kind`, `name`, `line`,
`visibility`, `module` и `signature`. Для типов, ссылок и диагностики
используйте `rust-analyzer`: индекс имена не резолвит.
