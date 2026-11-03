FROM eclipse-temurin:17-jre-jammy

# Minimal TLA+ model-checking court. Only a JRE, the pinned TLC jar, and
# coreutils for sha256 verification. No compiler, no shell tooling beyond the
# Ubuntu base.
ARG TLA_VERSION=v1.7.4
ARG TLA_SHA256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl coreutils jq \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/tla \
 && curl -fsSL --retry 3 \
      -o /opt/tla/tla2tools.jar \
      "https://github.com/tlaplus/tlaplus/releases/download/${TLA_VERSION}/tla2tools.jar" \
 && echo "${TLA_SHA256}  /opt/tla/tla2tools.jar" | sha256sum -c - \
 && printf 'TLA_VERSION=%s\nTLA_SHA256=%s\n' "${TLA_VERSION}" "${TLA_SHA256}" > /opt/tla/VERSION

ENV TLA_JAR=/opt/tla/tla2tools.jar \
    JAVA_TOOL_OPTIONS="-Xmx1536m -XX:MaxMetaspaceSize=256m -XX:+UseParallelGC"
WORKDIR /work
