# Протокол испытаний: Отказоустойчивый Control Plane K3s через kube-vip (VIP 198.18.1.30)

**Дата проведения:** 2026-11-20  
**Стенд:** 3x FriendlyElec NanoPi R5C (`k3s-1`, `k3s-2`, `k3s-3`), Rockchip RK3568B2 (4 ядра Cortex-A55, 4 ГБ RAM), Debian 13 trixie arm64, ядро `6.12.116+deb1Debian GNU/Linux 13 (trixie)`.  
**Версия K3s:** `v1.36.2+k3s1` (embedded etcd HA).  
**Компонент HA:** `kube-vip:v0.8.9` (Static Pods на каждом узле).  
**Сеть:** `wan0` (198.18.1.0/24), VIP: `198.18.1.30/32`.

---

## 1. Результаты внедрения

### 1.1. Статические поды kube-vip
- На всех трех узлах в `/var/lib/rancher/k3s/agent/pod-manifests/kube-vip.yaml` развернут манифест статического пода.
- Все поды поднялись в статусе `Running`:
  - `kube-vip-k3s-1` (IP `10.10.10.51`, hostNetwork: true) — `Running`
  - `kube-vip-k3s-2` (IP `10.10.10.52`, hostNetwork: true) — `Running`
  - `kube-vip-k3s-3` (IP `10.10.10.53`, hostNetwork: true) — `Running`

### 1.2. Конфигурация TLS SAN
- В `/etc/rancher/k3s/config.yaml` на всех узлах добавлен параметр:
  ```yaml
  tls-san:
    - "198.18.1.30"
  ```
- Сертификат `serving-kube-apiserver.crt` на каждом узле автоматически перегенерирован K3s при перезапуске и содержит `IP Address:198.18.1.30`.
- Запросы `curl -k` и `kubectl` к `https://198.18.1.30:6443` проходят валидацию без ошибок SAN.

---

## 2. Результаты теста отказоустойчивости (Failover Test)

### 2.1. Исходное состояние
- **Лидер Lease (`plndr-cp-lock`):** `k3s-1`
- **Держатель VIP `198.18.1.30`:** `k3s-1` (интерфейс `wan0`)
- **Статус `kubectl get nodes` через VIP:** `PASS` (3 узла `Ready`)

### 2.2. Имитация падения лидера
- Выполнена остановка службы `k3s`: `ssh mmv@k3s-1 "sudo systemctl stop k3s"`.
- **Время перехвата VIP:** < 4 секунд.
- **Новый лидер Lease:** `k3s-2`.
- **Новый держатель VIP:** `k3s-2` (`inet 198.18.1.30/32 scope global wan0`).
- **Поведение `kubectl get nodes`:** Запросы продолжили мгновенно возвращать результат без `connection refused` и без таймаутов, так как VIP уже обслуживался узлом `k3s-2`.

### 2.3. Восстановление узла
- Служба `k3s` запущена обратно на `k3s-1`: `ssh mmv@k3s-1 "sudo systemctl start k3s"`.
- Узел `k3s-1` успешно вернулся в статус `Ready`.
- VIP остался на `k3s-2` (отсутствие нежелательного флаппинга / flap-damping).

---

## 3. Выводы

1. Архитектура с `kube-vip` в режиме Static Pod + L2 ARP полностью устранила проблему потери связи при перезагрузках или плановом обслуживании отдельных мастеров кластера.
2. Локальный kubeconfig (`~/.kube/k3s-home.yaml`) и Freelens настроены на единую точку входа `https://198.18.1.30:6443`.
