# Дизайн: SNMP-мониторинг, дашборды Grafana и алертинг для роутера Netcraze Ultra

**Дата:** 2026-11-21  
**Статус:** Утверждён  

## Контекст и цель

На пограничном роутере домашней сети **Netcraze Ultra (`NC-1812`, `NDMS 5.01`, `198.51.100.1`)** включён встроенный SNMP-сервер (`service snmp`, SNMPv2c, UDP `161`, community `public`). Требуется организовать регулярный сбор метрик роутера в VictoriaMetrics на NAS (`198.51.100.105`), подготовить дашборды в Grafana и добавить базовые правила алертинга в `vmalert` / `Alertmanager`.

По результатам живого обследования SNMP-дерева роутера он отдаёт:
- `SNMPv2-MIB` (`sysDescr`, `sysUpTime`, `sysName` и др.);
- `IF-MIB` (64-битные счётчики `ifHCInOctets`, `ifHCOutOctets`, `ifHCInUcastPkts`, `ifHCOutUcastPkts`, ошибки, сбросы, `ifOperStatus`, `ifAdminStatus`, `ifHighSpeed` для WAN-интерфейсов `ISP1`/`ISP2`, GRE-туннелей `Gre0`/`Gre1`, мостов `Home`/`Guest`, Wi-Fi `AccessPoint*`/`GuestWiFi*` и физических портов `0..5`);
- `UCD-SNMP-MIB`: CPU (`ssCpuRaw*`), Load Average (`laTable` / `laLoadInt`), RAM (`memTotalReal`, `memAvailReal`, `memBuffer`, `memCached`), дисковая таблица USB/OPKG (`dskTable`);
- `TCP-MIB` и `UDP-MIB`: установленные соединения (`tcpCurrEstab`), сегменты, ретрансмиты, ошибки, 64-битные счётчики UDP-датаграмм.

## Архитектурные решения

### 1. Сбор метрик (`snmp-exporter` + VictoriaMetrics на NAS)

- **Размещение экспортера:** контейнер `snmp-exporter` (`prom/snmp-exporter:v0.31.0`, зафиксированный по digest) добавляется в Compose-стек [`ops/nas-monitoring/docker-compose.yaml`](../../../ops/nas-monitoring/docker-compose.yaml) на NAS `198.51.100.105`. Это не создаёт зависимости мониторинга роутера от доступности кластера k3s и минимизирует сетевую петлю.
- **Сетевая схема и безопасность:** порт `9116` публикуется только на `127.0.0.1:9116:9116` для локальной диагностики на NAS; VictoriaMetrics обращается к экспортеру по имени сервиса `snmp-exporter:9116` внутри сети Compose.
- **Конфигурация модулей:** используется встроенный в образ `/etc/snmp_exporter/snmp.yml` (профиль аутентификации `public_v2` и модули `system`, `if_mib`, `ucd_la_table`, `ucd_memory`, `ucd_system_stats`) в сочетании со вторым `--config.file=/etc/snmp_exporter/extra/netcraze-extra.yml` (каталог `ops/nas-monitoring/snmp-exporter/` монтируется целиком, чтобы избежать залипания inode при `scp`). Дополнительный файл определяет два компактных модуля:
  - `netcraze_disk` (`1.3.6.1.4.1.2021.9`, метрики `dskTotal`, `dskAvail`, `dskUsed`, `dskPercent`, `dskPercentNode` с заменой `dskIndex` на `dskPath`);
  - `netcraze_tcp_udp` (`1.3.6.1.2.1.6`, `1.3.6.1.2.1.7`, счётчики `TCP-MIB` и `UDP-MIB`).
- **Скрапинг VictoriaMetrics:** в [`ops/nas-monitoring/victoria-metrics/scrape.yaml`](../../../ops/nas-monitoring/victoria-metrics/scrape.yaml) добавляется job `netcraze-snmp` (`scrape_interval: 30s`, `scrape_timeout: 15s`), опрашивающий цель `198.51.100.1` (`instance="netcraze"`) сразу по всем 7 модулям через `params.module`.
- **Выкладка:** [`ops/nas-monitoring/deploy.sh`](../../../ops/nas-monitoring/deploy.sh) дополняется созданием и копированием каталога `snmp-exporter/*.yml`.

### 2. Дашборды Grafana

Создаются два тематических дашборда в [`ops/grafana/dashboards/`](../../../ops/grafana/dashboards/) в стиле проекта (`schemaVersion: 39`, `editable: false`, datasource `prometheus` с `uid: adnsc1wi03doga`, взаимные ссылки-переключатели в `links`):

1. **`Netcraze Router — System & WAN`** (`netcraze-system.json`, `uid: netcraze-system`):
   - *Сводка:* статус SNMP (`up`), Uptime, модель/прошивка (`sysDescr`), CPU %, реальная занятая RAM %, Load 1m, диск OPKG %, статусы `ISP1`/`ISP2` и `Gre0`/`Gre1`.
   - *WAN и GRE-туннели:* битрейт RX/TX (`ISP1`/`ISP2`, `Gre0`/`Gre1`) и PPS.
   - *Ресурсы системы:* CPU % по режимам (User/System/Nice), Load Average (`Load-1`/`5`/`15`), память stacked (`Used`, `Buffers`, `Cached`, `Free`, `Total`).
   - *Диск USB/OPKG и TCP/UDP:* место и Inodes на `/tmp/mnt/OPKG/` (с фильтрацией UUID-дубликата), `tcpCurrEstab` + открытия/ретрансмиты/ошибки TCP, UDP-датаграммы и ошибки.

2. **`Netcraze Router — Interfaces`** (`netcraze-interfaces.json`, `uid: netcraze-interfaces`):
   - Переменная фильтрации `$interface` (`ifName`, мультивыбор + `All`).
   - *Сводная таблица интерфейсов:* `ifIndex`, `ifName`, `ifAlias`, `ifDescr`, `ifOperStatus`, `ifHighSpeed`, текущий RX/TX (бит/с), ошибки/с, сбросы/с (все 6 запросов с единой группировкой `(ifIndex, ifName, ifAlias, ifDescr)` для корректного `merge`).
   - *LAN-мосты и Wi-Fi:* трафик `Home`/`Guest`, трафик Wi-Fi 2.4/5 ГГц (`AccessPoint*`/`GuestWiFi*`), ошибки и сбросы Wi-Fi.
   - *Физические порты `0..5`:* статус линка, скорость (`100`/`1000` Мбит/с) и трафик портов.
   - *Детальный анализ по `$interface`:* битрейт, PPS, ошибки/с и сбросы/с.

### 3. Алертинг (`vmalert` + `Alertmanager`)

В [`ops/alerting/vmalert/rules.yaml`](../../../ops/alerting/vmalert/rules.yaml) добавляется группа `netcraze` (`interval: 1m`, аннотации без `<`, `>`, `&` для Telegram HTML):

- `NetcrazeSnmpDown` (`critical`, `for: 3m`): `up{job="netcraze-snmp"} < 1` → `owner`.
- `NetcrazeRebooted` (`warning`, `for: 0s`): `sysUpTime{job="netcraze-snmp"} / 100 < 600` → маршрут `owner-events` в [`alertmanager.yml`](../../../ops/alerting/alertmanager/alertmanager.yml) (без уведомления `resolved`).
- `NetcrazeWanDown` (`warning`, `for: 2m`, `keep_firing_for: 10m`): `ifOperStatus{job="netcraze-snmp",ifName=~"ISP.*"} != 1 and on (instance, ifIndex) ifAdminStatus{job="netcraze-snmp"} == 1` → `owner` (игнорирует вручную отключённый резервный WAN).
- `NetcrazeMemoryHigh` (`warning`, `for: 15m`): эффективная занятая память (без `memBuffer` и `memCached`) > 85% → `owner`.
- `NetcrazeDiskHigh` (`warning`, `for: 15m`): `dskPercent > 85` с исключением UUID-алиаса `/tmp/mnt/<UUID>/` → `owner`.

Юнит-тесты добавляются в [`ops/alerting/vmalert/tests/netcraze.yaml`](../../../ops/alerting/vmalert/tests/netcraze.yaml) и подключаются в [`ops/alerting/test.sh`](../../../ops/alerting/test.sh) вместе с тестами маршрутизации `amtool`.

## Верификация и выкладка

1. **Офлайн-проверки:**
   - `prom/snmp-exporter --dry-run` на встроенном `snmp.yml` + `netcraze-extra.yml`;
   - `docker compose -f ops/nas-monitoring/docker-compose.yaml config -q`;
   - структурная JSON-валидация обоих дашбордов;
   - `./ops/alerting/test.sh`.
2. **Выкладка на NAS и в k3s:**
   - `./ops/nas-monitoring/deploy.sh --up` + проверка `snmp-exporter:9116`, `/targets` и PromQL-запросов VictoriaMetrics;
   - `./ops/grafana/dashboards/deploy.sh --nas netcraze-system.json netcraze-interfaces.json` + рестарт `grafana` и проверка логов провижининга и `/api/health`;
   - `./ops/alerting/deploy.sh` + reload всех 3 реплик Alertmanager + проверка группы `netcraze` в API `vmalert`.
