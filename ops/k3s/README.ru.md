# Регулярные задания обслуживания кластера (`ops/k3s`)

[English version](README.md)

В этой директории собраны автономные манифесты обслуживания кластера и регулярные задания CronJob:

- [`registry-gc-cronjob.yaml`](registry-gc-cronjob.yaml): Еженедельная сборка мусора (`registry garbage-collect --delete-untagged`) локального приватного реестра образов (`registry:2.8.3`) в namespace `registry` (по воскресеньям в 04:00 UTC). Освобождает неиспользуемые блобы слоев на NFS-бэкенде (`/volume1/k8s/registry`).
