# Стенд duovol

Три ВМ Lima (`dv-1`, `dv-2`, `dv-3`) с Debian 13 trixie arm64, ядром
`6.12.116+deb13-arm64`, k3s из трёх server и привилегированным DaemonSet
`toolbox` с образом `duovol-drbd` (модуль DRBD 9.3.4, `drbd-utils` 9.35.0).
Стенд нужен для проверок из `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.

## Требования

| Что | Значение |
|---|---|
| Хост | macOS на Apple silicon |
| Lima | 2.3.0, `vmType: vz`, сеть `user-v2` (`stand/lima/node.yaml`) |
| Docker | `docker buildx`, сборка под `linux/arm64` |
| Ресурсы | по 2 CPU, 3 ГиБ RAM и 20 ГиБ диска на ВМ |

## Порядок запуска

Команды выполняются из корня репозитория.

1. Соберите образ: `image/build.sh`. Переменные: `KVER` (по умолчанию
   `6.12.116+deb13-arm64`), `IMAGE` (по умолчанию `duovol-drbd:dev`).
2. Создайте ВМ: `stand/up.sh`. Скрипт запускает ВМ, при несовпадении ядра ставит
   `linux-image-$KVER`, ставит `nftables` и вызывает `stand/check.sh`. Ожидаемый
   конец вывода: `OK: stand is up`.
3. Поставьте k3s и toolbox: `stand/k3s.sh`. Скрипт кладёт чёрный список `drbd` в
   `/etc/modprobe.d`, ставит k3s версии `K3S_VERSION` (по умолчанию
   `v1.37.2+k3s1`), импортирует образ `IMAGE` в containerd каждого узла,
   перезапускает DaemonSet `toolbox` и загружает модуль. Повторный запуск не
   переустанавливает k3s и подхватывает пересобранный образ с тем же тегом.

Ветка установки ядра в `stand/up.sh` (`apt-get install linux-image-$KVER`,
`update-grub`, перезапуск ВМ) не выполнялась ни разу.

## Переменные

| Переменная | По умолчанию | Что задаёт |
|---|---|---|
| `NET_IF` | `eth0` | Интерфейс общей сети внутри ВМ; с него `vm_ip` берёт адрес узла |
| `KVER` | `6.12.116+deb13-arm64` | Версия ядра, которую проверяют `up.sh` и `check.sh` |
| `IMAGE` | `duovol-drbd:dev` | Образ, который `k3s.sh` импортирует в узлы |
| `K3S_VERSION` | `v1.37.2+k3s1` | Версия k3s для `INSTALL_K3S_VERSION` |

## Работа со стендом

`stand/lib.sh` содержит функции, которые подключаются через `source`:

| Функция | Что делает |
|---|---|
| `vm <узел> <команда>` | Выполняет команду от root внутри ВМ |
| `tb <узел> <команда>` | Выполняет команду в поде `toolbox` этого узла |
| `kc <аргументы kubectl>` | Запускает `kubectl` на первом узле из `NODES`, который в Lima в состоянии `Running` и чей API отвечает |

После перезапуска ВМ модуль DRBD не загружен. Загрузите его:

```bash
bash -c 'source stand/lib.sh; tb dv-2 "/opt/duovol/load-module.sh"'
```

Ресурсы DRBD, loop-устройства и вспомогательные скрипты проверок удаляет
`stand/spike/clean.sh`. ВМ и их диски удаляет `stand/down.sh`.
