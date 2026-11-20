# План реализации: Отказоустойчивый Control Plane K3s через kube-vip (VIP 198.18.1.30)

> **Для исполнителей:** ОБЯЗАТЕЛЬНЫЙ ПОД-НАВЫК: Используйте `superpowers:executing-plans` для последовательного выполнения задач по шагам с чекбоксами (`- [ ]`).

**Цель:** Развернуть отказоустойчивую единую точку входа (Virtual IP) `198.18.1.30:6443` на базе `kube-vip` для трех узлов NanoPi R5C (`k3s-1..3`), настроить TLS SAN и переключить локальный kubeconfig для бесперебойной работы `kubectl` и Freelens.

**Архитектура:** `kube-vip` развертывается как **Static Pod** (`/var/lib/rancher/k3s/agent/pod-manifests/kube-vip.yaml`) на всех 3 узлах, работая на интерфейсе `wan0` в режиме L2 (Gratuitous ARP) с leader election через Kubernetes Lease `plndr-cp-lock`. В K3s добавляется `tls-san: 198.18.1.30`.

**Стек технологий:** K3s, kube-vip v0.8.9, Kubernetes Lease API, systemd, bash.

## Global Constraints

- Целевые узлы: `k3s-1` (198.18.1.51), `k3s-2` (198.18.1.52), `k3s-3` (198.18.1.53).
- Пользователь SSH: `mmv` (доступ с sudo без пароля).
- Сетевой интерфейс для VIP: `wan0` (сеть 198.18.1.0/24).
- Выделенный VIP: `198.18.1.30/32`.
- Порт API: `6443`.
- Режим: строго Control Plane (`cp_enable: "true"`, `svc_enable: "false"` во избежание конфликтов с MetalLB).
- Все скрипты и манифесты оформляются в `stand/` репозитория ambar для воспроизводимости.

---

### Задача 1: Добавление TLS SAN 198.18.1.30 в конфигурацию K3s на всех узлах

**Файлы:**
- Создать/Модифицировать на узлах: `/etc/rancher/k3s/config.yaml`
- Модифицировать в репо: `stand/r5c-install-k3s.sh` (добавить VIP в шаблон/скрипт)

**Интерфейсы:**
- Производит: валидный TLS-сертификат API-сервера K3s, содержащий SAN `IP Address:198.18.1.30`.

- [ ] **Шаг 1: Добавить `tls-san: ["198.18.1.30"]` в `/etc/rancher/k3s/config.yaml` на `k3s-1`, `k3s-2`, `k3s-3`**
- [ ] **Шаг 2: Выполнить последовательный rolling-restart `k3s.service` по очереди на каждом узле**
  - Перезапустить `k3s` на `k3s-1`, дождаться `systemctl is-active k3s` и `Ready` в `kubectl get nodes`.
  - Перезапустить `k3s` на `k3s-2`, дождаться `Ready`.
  - Перезапустить `k3s` на `k3s-3`, дождаться `Ready`.
- [ ] **Шаг 3: Проверить наличие SAN `198.18.1.30` в сертификате на каждом узле**
  - Выполнить проверку через `openssl x509 -in /var/lib/rancher/k3s/server/tls/serving-kube-apiserver.crt -text -noout`.

---

### Задача 2: Подготовка и развертывание манифеста Static Pod kube-vip

**Файлы:**
- Создать в репо: `stand/kube-vip.yaml.template`
- Создать в репо: `stand/r5c-kube-vip.sh` (скрипт деплоя и валидации kube-vip на кластере)
- Создать на узлах: `/var/lib/rancher/k3s/agent/pod-manifests/kube-vip.yaml`

**Интерфейсы:**
- Потребляет: TLS SAN 198.18.1.30, `/etc/rancher/k3s/k3s.yaml` на узлах.
- Производит: работающие статические поды `kube-vip` на всех узлах, активный Lease `plndr-cp-lock` в namespace `kube-system`.

- [ ] **Шаг 1: Создать шаблон манифеста `stand/kube-vip.yaml.template`**
  - Контейнер `ghcr.io/kube-vip/kube-vip:v0.8.9`.
  - Переменные: `vip_arp="true"`, `port="6443"`, `vip_interface="wan0"`, `address="198.18.1.30"`, `cp_enable="true"`, `cp_namespace="kube-system"`, `vip_leaderelection="true"`, `vip_leasename="plndr-cp-lock"`.
  - Volume mount: `/etc/rancher/k3s/k3s.yaml` -> `/etc/kubernetes/admin.conf`.
  - `hostNetwork: true`.
- [ ] **Шаг 2: Создать скрипт автоматизации `stand/r5c-kube-vip.sh`**
  - Раскладка манифеста по узлам через ssh/sudo.
  - Проверка статуса подов kube-vip.
- [ ] **Шаг 3: Разложить манифест на `k3s-1`, `k3s-2`, `k3s-3`**
  - Скопировать в `/var/lib/rancher/k3s/agent/pod-manifests/kube-vip.yaml`.
- [ ] **Шаг 4: Проверить статус подов и лидера**
  - `kubectl get pods -n kube-system | grep kube-vip`
  - `kubectl get lease -n kube-system plndr-cp-lock -o yaml`
- [ ] **Шаг 5: Закоммитить созданные файлы в git**

---

### Задача 3: Верификация виртуального IP и обновление клиентского kubeconfig

**Файлы:**
- Модифицировать: `/Users/mvmalykh/.kube/k3s-home.yaml`

**Интерфейсы:**
- Потребляет: активный VIP 198.18.1.30 на лидере.
- Производит: рабочий kubeconfig с единой отказоустойчивой точкой входа.

- [ ] **Шаг 1: Проверить доступность VIP 198.18.1.30 с хоста разработки**
  - `ping -c 2 198.18.1.30`
  - `curl -k https://198.18.1.30:6443/version`
  - `kubectl --server https://198.18.1.30:6443 get nodes`
- [ ] **Шаг 2: Обновить `/Users/mvmalykh/.kube/k3s-home.yaml`**
  - Кластер `default` -> `server: https://198.18.1.30:6443`.
  - Сохранить резервные контексты `k3s-1`, `k3s-2`, `k3s-3`.
- [ ] **Шаг 3: Проверить работу Freelens / kubectl без указания контекста**
  - `kubectl get nodes` (проверка соединения с сервером по умолчанию).

---

### Задача 4: Стресс-тест отказоустойчивости (Failover Test)

**Файлы:**
- Создать в репо: `docs/superpowers/specs/2026-11-20-kube-vip-ha-results.md` (протокол испытаний)

**Интерфейсы:**
- Потребляет: работающий HA-кластер под управлением `kube-vip`.
- Производит: подтверждение автоматического перехвата VIP при отказе лидера.

- [ ] **Шаг 1: Определить текущий узел-лидер Lease `plndr-cp-lock`**
- [ ] **Шаг 2: Симулировать падение лидера**
  - Выполнить `sudo systemctl stop k3s` на лидере.
- [ ] **Шаг 3: Замерить время перехвата VIP и проверить `kubectl`**
  - Убедиться, что Lease перешел на другой узел в течение 5 секунд.
  - Выполнить `kubectl get nodes` — запросы должны продолжать успешно выполняться через `198.18.1.30:6443`.
- [ ] **Шаг 4: Восстановить узел**
  - Выполнить `sudo systemctl start k3s` на остановленном узле.
  - Убедиться в возвращении узла в статус `Ready`.
- [ ] **Шаг 5: Оформить результаты в отчете и закоммитить**
