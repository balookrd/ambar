# Cluster Maintenance Jobs (`ops/k3s`)

[Русская версия](README.ru.md)

This directory contains standalone Kubernetes maintenance manifests and cron jobs for the cluster:

- [`registry-gc-cronjob.yaml`](registry-gc-cronjob.yaml): Weekly garbage collector (`registry garbage-collect --delete-untagged`) for the private OCI registry (`registry:2.8.3`), running in namespace `registry` on Sundays at 04:00 UTC. It reclaims unreferenced blob storage from the shared `registry-data` PVC (`ambar-r3-retain`, `ReadWriteOnce`), using `podAffinity` to `app: registry` so the job runs on the same node as the active registry pod.
