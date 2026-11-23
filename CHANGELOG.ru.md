# История изменений

Начиная с `v26.11.0`, `ambar` использует схему версионирования CalVer формата
`vYY.M.PATCH` без ведущего нуля у месяца (например, `v26.11.0`, `v26.11.1`,
`v26.12.0`). До первого релиза в `Cargo.toml` использовалась предрелизная
версия `0.1.0`. Версия схемы Kubernetes CRD (`v1alpha1` в `ReplicatedVolume`)
развивается независимо от релизных тегов драйвера.

Заметки к каждому GitHub Release формируются автоматически из коммитов формата
Conventional Commits по всему репозиторию с помощью [`cliff.toml`](cliff.toml) и
[`.github/scripts/gen-release-notes.sh`](.github/scripts/gen-release-notes.sh)
(включаются `feat`, `fix`, `perf` и `BREAKING CHANGE`; коммиты с типами `ops`,
`docs`, `chore`, `refactor`, `test`, `ci`, `build`, `bench` и `style`
пропускаются).

## Базовое состояние до перехода на CalVer (фазы 0–5)

К моменту перехода на CalVer в проекте завершены фазы 0–5 и аудит безопасности:

- **CSI-драйвер Kubernetes поверх DRBD 9:** сервисы Identity, Controller и Node,
  цикл согласования агента, CRD `ReplicatedVolume` (`v1alpha1`) и набор
  модульных и e2e-тестов.
- **Управление репликами на лету:** команды CLI `ambar replica add`, `remove` и
  `move` с проверками безопасности и перебалансировкой свидетеля.
- **Обработка отказов и наблюдаемость:** автоматический taint `no-quorum`,
  выселение подов после 30-секундного grace period, авто-восстановление
  `StandAlone`-соединений на Secondary (`drbdadm adjust`), детектирование
  split-brain, очистка неиспользуемых `Client` и метрики Prometheus на
  `:9770/metrics`.
- **Прямой блочный бэкенд LVM (`--volume-group`):** работа напрямую с
  логическими томами без loopback-файлов и двойного журналирования ext4/JBD2,
  ускорившая медианную задержку 4K `fsync` на кластере NanoPi R5C в 2.4–2.8 раза.
- **Укрепление безопасности и конкурентности:** файловые блокировки томов в CSI
  и цикле обработки отказов, атомарные JSON Patch-обновления в Kubernetes API с
  проверкой `resourceVersion`, сверка PVC-к-PV при выселении подов, защита от
  path traversal и ограничение параллелизма HTTP-сервера метрик.

*English version: [CHANGELOG.md](CHANGELOG.md)*
