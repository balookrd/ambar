# Настройка рабочего внешнего URL для vmalert и Alertmanager

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Настроить внешние URL (`-external.url=https://vmalert.k3s.beerloga.su` для `vmalert` и `--web.external-url=https://alertmanager.k3s.beerloga.su` для `alertmanager`), чтобы ссылки в уведомлениях (строка `Source:` в Telegram/email, кнопки глушения `silence`) вели на доступный HTTPS Ingress вместо внутреннего hostname пода.

**Architecture:**
- В манифест Deployment `vmalert` добавляется флаг запуска `-external.url=https://vmalert.k3s.beerloga.su`. vmalert использует его при конструировании поля `generatorURL` для всех отправляемых в Alertmanager алертов.
- В манифест StatefulSet `alertmanager` добавляется флаг запуска `--web.external-url=https://alertmanager.k3s.beerloga.su`. Alertmanager использует его при формировании ссылок на silences и веб-интерфейс.
- Изменения синхронизируются в обоих репозиториях (`outline-proxy` и `ambar`).
- Манифесты применяются в кластер k3s через `kubectl apply`.

**Tech Stack:** Kubernetes (k3s), VictoriaMetrics (vmalert v1.155.0), Prometheus Alertmanager v0.34.2, Traefik Ingress.

## Global Constraints

- Язык всей документации, планов и коммитов: русский для документации/планов, английский для кода и git commit messages.
- Перед применением в кластер обязателен локальный `--dry-run=server`.
- Не ломать существующие проверки и тесты (`./ops/alerting/test.sh`).
- Сохранять синхронность файлов между `outline-proxy` и `ambar`.

---

### Task 1: Обновление манифестов k3s и документации

**Files:**
- Modify: `ops/nanopi-r5c-k3s/apps/monitoring/vmalert.yaml`
- Modify: `ops/nanopi-r5c-k3s/apps/monitoring/alertmanager.yaml`
- Modify: `/Users/mvmalykh/IdeaProjects/ambar/ops/nanopi-r5c-k3s/apps/monitoring/vmalert.yaml`
- Modify: `/Users/mvmalykh/IdeaProjects/ambar/ops/nanopi-r5c-k3s/apps/monitoring/alertmanager.yaml`

- [ ] **Step 1: Добавить `-external.url` в `vmalert.yaml`**
Добавить `- -external.url=https://vmalert.k3s.beerloga.su` в список `args` контейнера `vmalert` в обоих репозиториях.

- [ ] **Step 2: Добавить `--web.external-url` в `alertmanager.yaml`**
Добавить `- --web.external-url=https://alertmanager.k3s.beerloga.su` в список `args` контейнера `alertmanager` в обоих репозиториях.

- [ ] **Step 3: Запустить локальные тесты алертинга**
Выполнить `./ops/alerting/test.sh` и убедиться, что тесты правил и конфигурации Alertmanager проходят зелёными.

---

### Task 2: Проверка и применение манифестов в кластере k3s

**Files:**
- Apply: `ops/nanopi-r5c-k3s/apps/monitoring/vmalert.yaml`
- Apply: `ops/nanopi-r5c-k3s/apps/monitoring/alertmanager.yaml`

- [ ] **Step 1: Выполнить dry-run проверку через kubectl**
```bash
KUBECONFIG=~/.kube/k3s-home.yaml kubectl apply --dry-run=server \
  -f ops/nanopi-r5c-k3s/apps/monitoring/vmalert.yaml \
  -f ops/nanopi-r5c-k3s/apps/monitoring/alertmanager.yaml
```

- [ ] **Step 2: Применить обновлённые манифесты**
```bash
KUBECONFIG=~/.kube/k3s-home.yaml kubectl apply \
  -f ops/nanopi-r5c-k3s/apps/monitoring/vmalert.yaml \
  -f ops/nanopi-r5c-k3s/apps/monitoring/alertmanager.yaml
```

- [ ] **Step 3: Дождаться завершения rollout подов**
```bash
KUBECONFIG=~/.kube/k3s-home.yaml kubectl -n monitoring rollout status deploy/vmalert --timeout=60s
KUBECONFIG=~/.kube/k3s-home.yaml kubectl -n monitoring rollout status statefulset/alertmanager --timeout=120s
```

---

### Task 3: Верификация работы ссылок

- [ ] **Step 1: Проверить поле source через API vmalert**
Запросить текущие алерты из `https://vmalert.k3s.beerloga.su/api/v1/alerts`:
```bash
curl -s https://vmalert.k3s.beerloga.su/api/v1/alerts | jq '.data.alerts[0].source'
```
Убедиться, что `source` начинается с `https://vmalert.k3s.beerloga.su/vmalert/alert?...` вместо `http://vmalert-...:8880/...`.

- [ ] **Step 2: Проверить HTTP-доступность ссылки**
Выполнить curl по полученному URL `source` и убедиться в ответе `HTTP/2 200`.
