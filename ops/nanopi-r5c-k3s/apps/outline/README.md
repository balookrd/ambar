# outline на кластере

`outline-server` (сервер data plane) и `outline-client` (клиент SOCKS5/TUN) работают
без PVC: состояния на диске нет, конфиг через ConfigMap, секреты через Secret. Открыт
только вопрос размещения.

## Развилка размещения (TODO — подтвердить)

Оба бинаря сетевые, поэтому размещение определяет не CPU, а к чему привязан трафик:

- Одинаковая роль на всех нодах (например, `outline-server` как edge на каждой) →
  `DaemonSet`, `hostNetwork: true`.
- Привязка к конкретному uplink конкретной ноды (`outline-client` поднимает TUN к
  определённому аплинку одной платы) → `Deployment` с `nodeSelector`/`nodeAffinity`
  на эту ноду, `replicas: 1`.

Скелеты в каталоге:

| Файл | Объект | Что уже задано |
|---|---|---|
| `outline-server.daemonset.yaml` | `DaemonSet` | `hostNetwork: true`, `NET_ADMIN` (SO_MARK / fwmark), ConfigMap `outline-server-config` + Secret `outline-server-secrets` |
| `outline-client.deployment.yaml` | `Deployment` | `hostNetwork: true`, `nodeSelector` на `k3s-1`, `NET_ADMIN`, `/dev/net/tun` через `hostPath`, ConfigMap `outline-client-config` + Secret `outline-client-secrets` |

Образ (`<REGISTRY>/…:<TAG>`) и конфиг в ConfigMap — заглушки, поэтому `deploy.sh`
оба файла пропускает. После выбора варианта лишний файл удалить.

## Требования к подам

- `securityContext.capabilities.add: [NET_ADMIN]` — обоим (fwmark-роутинг; у `outline-client`
  ещё и TUN ingress);
- `/dev/net/tun` через `hostPath` или device-plugin — для TUN-пути `outline-client`;
- `hostNetwork: true`, если нужен доступ к физическому uplink/fwmark-роутингу;
- конфиг — ConfigMap, секреты (PSK/UUID/токены) — Secret вне git.

Не логировать PSK/UUID/токены (инвариант репо), metrics labels — low-cardinality.
