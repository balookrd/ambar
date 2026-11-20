# Infrastructure and Operations (ops)

[Русская версия](README.ru.md)

This directory contains operations documentation, Kubernetes workload manifests, monitoring configurations, and automation scripts for the home production infrastructure running on the 3x FriendlyElec NanoPi R5C cluster and supporting NAS.

## Directory Structure

```
ops/
├── nanopi-r5c-k3s/      # NanoPi R5C hardware runbook, Debian bootstrap, and k3s apps
│   ├── README.md        # Complete hardware provisioning and OS setup guide
│   └── apps/            # Kubernetes workload manifests and deploy.sh
│       ├── descheduler/ # Pod balancing CronJob
│       ├── home/        # Home services (mosquitto, zigbee2mqtt, jellyfin)
│       ├── ingress/     # Traefik, MetalLB, TLS Store, certificate publishing
│       ├── monitoring/  # vmalert, alertmanager, node-exporter, outline-ui
│       ├── outline/     # Outline server & client daemonsets/deployments
│       ├── registry/    # Local OCI Docker registry
│       ├── smarthome/   # Smarthome microservices
│       ├── storage/     # NFS provisioner (historical/fallback storage)
│       └── vpn/         # AnyConnect (ocserv) VPN gateway
├── alerting/            # Alertmanager configs, vmalert rules, and rule unit tests
├── grafana/             # Grafana dashboards, alert policies, and provisioning
├── k3s/                 # Cluster maintenance CronJobs (registry-gc)
├── nas-monitoring/      # Docker Compose stack for NAS (VictoriaMetrics, Grafana, backups)
└── nas-jellyfin-backup/ # Incremental Jellyfin backup scripts for NAS
```

## Storage Integration with ambar

The cluster workloads migrated from pure NFS/local storage to `ambar` replicated block storage:
- CSI Driver manifests are maintained in [`deploy/`](../deploy/) (`ambar-controller`, `ambar-agent`, CRDs, and StorageClasses).
- StorageClasses: `ambar-r2` (2 replicas + witness) and `ambar-r3` (3 full replicas) backed by LVM volume group `k3s-vg` on NVMe.
- Physical node provisioning and benchmark scripts reside in [`stand/`](../stand/) (`r5c-*.sh`).
- Control plane high availability is provided by `kube-vip` (VIP `198.18.1.30:6443`, managed via [`stand/r5c-kube-vip.sh`](../stand/r5c-kube-vip.sh)).

## Quick Start

- **Deploy all apps:**
  ```bash
  ops/nanopi-r5c-k3s/apps/deploy.sh
  ```
- **Test and deploy alerting rules:**
  ```bash
  ops/alerting/test.sh
  ops/alerting/deploy.sh
  ```
- **Inspect cluster status:**
  ```bash
  stand/r5c-kube-vip.sh status
  kubectl get nodes -o wide
  ```
