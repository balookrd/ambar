# Инфраструктура и эксплуатация (ops)

[English version](README.md)

В этой директории собрана эксплуатационная документация, манифесты приложений Kubernetes, конфигурации мониторинга и скрипты автоматизации боевой домашней инфраструктуры, работающей на кластере из 3x FriendlyElec NanoPi R5C и вспомогательном хранилище NAS.

## Структура директорий

```
ops/
├── nanopi-r5c-k3s/      # Документация железа NanoPi R5C, bootstrap ОС и манифесты приложений k3s
│   ├── README.md        # Исчерпывающий гайд по подготовке плат, eMMC/NVMe и установке Debian johang
│   └── apps/            # Дерево манифестов приложений и deploy.sh
│       ├── descheduler/ # CronJob балансировки подов по нодам
│       ├── home/        # Домашние сервисы (mosquitto, zigbee2mqtt, jellyfin)
│       ├── ingress/     # Traefik Ingress, MetalLB pool, TLS Store, выгрузка сертификатов
│       ├── monitoring/  # vmalert, alertmanager, node-exporter, outline-ui
│       ├── outline/     # DaemonSet и Deployment узлов outline в k3s
│       ├── registry/    # Локальный OCI Docker registry
│       ├── smarthome/   # Микросервисы автоматизации умного дома
│       ├── storage/     # NFS провиженер (историческое / вспомогательное хранилище)
│       └── vpn/         # VPN шлюз AnyConnect (ocserv)
├── alerting/            # Конфигурация Alertmanager, правила vmalert и юнит-тесты алертов
├── grafana/             # Дашборды Grafana, политики алертинга и provisioning
├── k3s/                 # Регулярные задания кластера (registry-gc-cronjob)
├── nas-monitoring/      # Стек Docker Compose на NAS (VictoriaMetrics, Grafana, бэкапы)
└── nas-jellyfin-backup/ # Скрипты инкрементального бэкапа медиатеки Jellyfin на NAS
```

## Интеграция с драйвером ambar

Прикладная нагрузка кластера переведена на реплицируемое блочное хранилище `ambar`:
- Манифесты самого CSI-драйвера находятся в [`deploy/`](../deploy/) (`ambar-controller`, `ambar-agent`, CRD `ReplicatedVolume` и StorageClasses).
- Классы хранилища: `ambar-r2` (2 копии + witness) и `ambar-r3` (3 дисковые копии) поверх LVM volume group `k3s-vg` на NVMe.
- Скрипты подготовки физических узлов и бенчмарков собраны в [`stand/`](../stand/) (`r5c-*.sh`).
- Отказоустойчивый Control Plane обеспечивается `kube-vip` (VIP `198.18.1.30:6443`, автоматизирован через [`stand/r5c-kube-vip.sh`](../stand/r5c-kube-vip.sh)).

## Быстрый старт

- **Выкатка всех приложений кластера:**
  ```bash
  ops/nanopi-r5c-k3s/apps/deploy.sh
  ```
- **Тестирование и выкатка правил алертинга:**
  ```bash
  ops/alerting/test.sh
  ops/alerting/deploy.sh
  ```
- **Проверка статуса кластера и VIP:**
  ```bash
  stand/r5c-kube-vip.sh status
  kubectl get nodes -o wide
  ```
