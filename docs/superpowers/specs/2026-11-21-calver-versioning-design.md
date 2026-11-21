# Дизайн перехода `ambar` на версионирование CalVer (`vYY.M.PATCH`)

**Дата:** 2026-11-21
**Статус:** утверждён

## Цель и контекст

Перевести проект `ambar` на схему календарного версионирования CalVer `vYY.M.PATCH` (по образцу `outline-proxy`) и автоматизировать подготовку релизов, сборку статических бинарников под Linux (`musl`) и публикацию GitHub Releases с автогенерируемыми заметками к релизам.

## Ключевые решения

### 1. Схема версионирования
- **Формат версии в манифестах Cargo (`Cargo.toml` и `Cargo.lock`):** `YY.M.PATCH` без ведущего нуля у месяца (например, `26.11.0`, `26.11.1`, `26.12.0`). Этот формат полностью совместим с SemVer, требуемым Cargo, и автоматически попадает в `ambar --version` (`clap`) и CSI `GetPluginInfoResponse.vendor_version`.
- **Формат git-тегов:** `vYY.M.PATCH` (например, `v26.11.0`).
- **Независимость API CRD:** версия схемы ресурса `ReplicatedVolume` (`v1alpha1` в `src/crd.rs` и `deploy/crd.json`) остаётся независимой от релизных версий драйвера и не изменяется.
- **Стартовое состояние:** до первого запуска релизного workflow в `Cargo.toml` и `Cargo.lock` сохраняется предрелизная версия `0.1.0`. Первый переход на `26.11.0` выполняется самим workflow `Release` коммитом `release: cut v26.11.0`. Локальные стендовые и сборочные скрипты (`image/build.sh`, `stand/install.sh`, `stand/r5c-lib.sh`) сохраняют дефолтный тег образа `ambar-drbd:dev`.

### 2. Процесс выпуска релизов (GitHub Actions)
1. **`.github/workflows/release.yml`** (ручной запуск `workflow_dispatch`):
   - Принимает необязательный параметр `series` (`YY.M`; по умолчанию текущий месяц UTC `date -u +%-y.%-m`), валидирует формат `^[0-9]{2,}\.([1-9]|1[0-2])$`.
   - По существующим тегам `v${SERIES}.*` вычисляет следующий номер `PATCH` (начиная с `0`, если тегов этой серии ещё нет) и проверяет отсутствие коллизии.
   - Обновляет `version` в `Cargo.toml` и `Cargo.lock` с помощью `.github/scripts/version.py set` и проверяет согласованность через `.github/scripts/version.py check`.
   - Создаёт коммит `release: cut v<version>` от `github-actions[bot]`, отправляет его в `main` и выводит команды для локального создания подписанного тега `git tag -s v<version> -m "v<version>" && git push origin v<version>`.
2. **`.github/workflows/tag-release.yml`** (триггер `push` тегов `v[0-9]*.[0-9]*.[0-9]*`):
   - **Job `verify`:** проверяет соответствие тега формату `vYY.M.PATCH` и сверяет версию в `Cargo.toml` и `Cargo.lock` со значением `${tag#v}` до запуска сборочной матрицы.
   - **Job `build`:** параллельно собирает статические релизные бинарники `ambar` (`cargo zigbuild --release --locked --target <target>`) для `aarch64-unknown-linux-musl` и `x86_64-unknown-linux-musl`, упаковывает их в `ambar-<version>-<target>.tar.gz`. Контейнерный образ `ambar-drbd`, привязанный к конкретному ядру `KVER`, продолжает собираться локально через `image/build.sh`.
   - **Job `publish`:** скачивает артефакты обеих архитектур, вычисляет `SHA256SUMS.txt`, генерирует заметки к релизу с помощью `git-cliff` и `.github/scripts/gen-release-notes.sh` и публикует GitHub Release `ambar v<version>` (`make_latest: true`).

### 3. Генерация release notes (`git-cliff`)
- Используется конфигурация `cliff.toml` с фильтром по всему репозиторию (без фильтрации по путям): в release notes включаются `breaking` (`⚠️ BREAKING CHANGES`), `feat` (`⭐ Features`), `fix` (`🐛 Bug Fixes`) и `perf` (`⚡ Performance`). Коммиты типов `ops`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `bench`, `style` и `Merge` пропускаются.
- Скрипт `.github/scripts/gen-release-notes.sh <tag> <output-file> [end-ref]` находит предыдущий CalVer-тег строго меньше целевого (либо корневой коммит для самого первого релиза) и формирует ссылку `**Full Changelog**: .../compare/<from>...<to>` и безопасный fallback при отсутствии пользовательских изменений.

### 4. Проверки в CI
- `.github/workflows/ci.yml` дополняется проверкой синтаксиса `.github/scripts/*.sh` (`bash -n`), YAML-валидацией `.github/workflows/*.yml` и запуском `python3 .github/scripts/version.py self-test`.
