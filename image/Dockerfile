# syntax=docker/dockerfile:1

ARG KVER=6.12.116+deb13-arm64
ARG DRBD_TAG=drbd-9.3.4
ARG UTILS_TAG=v9.35.0

# --- kernel module -----------------------------------------------------------
FROM debian:trixie AS module
ARG KVER
ARG DRBD_TAG
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      git ca-certificates build-essential kmod coccinelle python3 \
      "linux-headers-${KVER}" \
 && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${DRBD_TAG}" --recursive --shallow-submodules \
      https://github.com/LINBIT/drbd /src
# SPAAS=false: generate the compat patch with local coccinelle, no network calls.
RUN make -j"$(nproc)" -C /src/drbd KDIR="/lib/modules/${KVER}/build" SPAAS=false
RUN mkdir -p "/out/${KVER}" \
 && cp "/src/drbd/build-${KVER}/drbd.ko" \
       "/src/drbd/build-${KVER}/drbd_transport_tcp.ko" "/out/${KVER}/"

# --- userland tools ----------------------------------------------------------
FROM debian:trixie AS utils
ARG UTILS_TAG
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      git ca-certificates build-essential autoconf automake flex \
      libkeyutils-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${UTILS_TAG}" --recursive --shallow-submodules \
      https://github.com/LINBIT/drbd-utils /src
WORKDIR /src
RUN ./autogen.sh \
 && ./configure --prefix=/usr --localstatedir=/var --sysconfdir=/etc \
      --without-84support --without-manual --without-heartbeat \
      --without-pacemaker --without-rgmanager --without-bashcompletion \
      --with-udev=no --with-initscripttype=none \
 && make -j"$(nproc)" \
 && make install DESTDIR=/stage

# --- runtime -----------------------------------------------------------------
FROM debian:trixie-slim
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      kmod util-linux e2fsprogs libkeyutils1 lvm2 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=module /out/ /opt/ambar/modules/
COPY --from=utils /stage/usr/sbin/drbdadm /stage/usr/sbin/drbdsetup \
     /stage/usr/sbin/drbdmeta /usr/sbin/
COPY drbd.conf /etc/drbd.conf
COPY --chmod=0755 load-module.sh /opt/ambar/load-module.sh
COPY modprobe-blacklist.conf /etc/modprobe.d/ambar-blacklist-drbd.conf
COPY --chmod=0755 ambar /usr/local/bin/ambar
RUN mkdir -p /etc/drbd.d /var/lib/drbd /var/run/drbd
