FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \ 
    ca-certificates \
    build-essential \
    make \
    curl \
    git \
    python3 \
    wget \
    gcc-arm-linux-gnueabihf \
    g++-arm-linux-gnueabihf \
    gcc-aarch64-linux-gnu \
    g++-aarch64-linux-gnu \
    ninja-build \
    neovim \
    openssh-client \
    rsync \
    file \
    libclang-dev \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /opt/

CMD ["/bin/bash"]

