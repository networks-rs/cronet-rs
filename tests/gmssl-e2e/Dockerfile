FROM rust:1.85-bookworm

ARG GMSSL_VERSION=v3.2.0
ARG NGINX_GMSSL_REVISION=7c70da7686ac4991b78da122d458682734721918
ARG GMSSL_ABI_CFLAGS="-DENABLE_AES -DENABLE_SHA2 -DENABLE_TLS"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential cmake git libpcre2-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN git clone --depth 1 --branch "$GMSSL_VERSION" \
        https://github.com/guanzhi/GmSSL.git /opt/GmSSL \
    && cmake -S /opt/GmSSL -B /opt/GmSSL/build \
        -DCMAKE_BUILD_TYPE=Release \
        -DENABLE_TLS=ON \
        -DENABLE_SHA1=OFF \
        -DENABLE_SHA2=ON \
        -DENABLE_AES=ON \
        -DENABLE_AES_CCM=OFF \
        -DENABLE_CHACHA20=OFF \
        -DENABLE_SM4_ECB=OFF \
        -DENABLE_SM4_OFB=OFF \
        -DENABLE_SM4_CFB=OFF \
        -DENABLE_SM4_CCM=OFF \
        -DENABLE_SM4_XTS=OFF \
        -DENABLE_SM4_CBC_MAC=OFF \
        -DENABLE_SM9=OFF \
        -DENABLE_CMS=OFF \
        -DENABLE_SECP256R1=OFF \
        -DENABLE_LMS=OFF \
        -DENABLE_XMSS=OFF \
        -DENABLE_SPHINCS=OFF \
        -DENABLE_KYBER=OFF \
        -DENABLE_ZUC=OFF \
        -DENABLE_GHASH=OFF \
        -DENABLE_SKF=OFF \
        -DENABLE_SDF=OFF \
    && cmake --build /opt/GmSSL/build --parallel 4 \
    && cmake --install /opt/GmSSL/build \
    && ldconfig

COPY tests/gmssl-e2e/nginx-gmssl-e2e.patch /tmp/nginx-gmssl-e2e.patch

RUN git clone https://github.com/GmSSL/nginx-gmssl.git /opt/nginx-gmssl \
    && git -C /opt/nginx-gmssl checkout "$NGINX_GMSSL_REVISION" \
    && git -C /opt/nginx-gmssl apply /tmp/nginx-gmssl-e2e.patch \
    && cd /opt/nginx-gmssl \
    && ./configure \
        --with-http_ssl_module \
        --with-gmssl=/usr/local \
        --with-cc-opt="$GMSSL_ABI_CFLAGS" \
        --with-ld-opt="-Wl,-rpath,/usr/local/lib" \
    && make -j4

RUN apt-get update \
    && apt-get install -y --no-install-recommends libclang-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . /work
WORKDIR /work

RUN cargo xtask sync --api-only --source-dir /opt/cronet/chromium/src

ENV CRONET_SYS_NO_LINK=1
ENV CRONET_SOURCE_DIR=/opt/cronet/chromium/src
ENV GMSSL_DIR=/usr/local
ENV GMSSL_CFLAGS="-DENABLE_AES -DENABLE_SHA2 -DENABLE_TLS"
ENV LD_LIBRARY_PATH=/usr/local/lib

CMD ["tests/gmssl-e2e/in-container.sh"]
