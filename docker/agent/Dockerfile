# docs/specs/docker.md: the agent in a Linux container. Built from the repo root:
#   docker compose -f docker/agent/compose.yaml up -d --build

ARG GLAB_VERSION=1.51.0
ARG PLAYWRIGHT_VERSION=1.63.0

# The agent's own executable, cross-compiled from source: this port has no release-bundle
# self-update yet (installers/03-the-bundle-agent-bundle.md section 3.12 isn't ported), so a newer
# agent means rebuilding this image, not something start.sh fetches.
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release --bin retaskable

# Node stays the base image: the Claude Code CLI the agent drives needs it, and so do the
# projects the agent builds and tests.
FROM node:22-bookworm-slim
ARG GLAB_VERSION
ARG PLAYWRIGHT_VERSION

# Claude Code's own updater is off: its install here is root's, and a new one comes with a
# rebuild of this image.
ENV DISABLE_AUTOUPDATER=1 \
    CLAUDE_CONFIG_DIR=/home/node/.claude \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright

RUN apt-get update && apt-get install -y --no-install-recommends \
      git curl ca-certificates gnupg python3 tini \
    && mkdir -p -m 0755 /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
         -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
         > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh \
    && curl -fsSL -o /tmp/glab.deb \
         "https://gitlab.com/gitlab-org/cli/-/releases/v${GLAB_VERSION}/downloads/glab_${GLAB_VERSION}_linux_$(dpkg --print-architecture).deb" \
    && apt-get install -y --no-install-recommends /tmp/glab.deb \
    && rm /tmp/glab.deb \
    && npm install -g @anthropic-ai/claude-code \
    && npx -y "playwright@${PLAYWRIGHT_VERSION}" install --with-deps chromium \
    && chmod -R a+rX "$PLAYWRIGHT_BROWSERS_PATH" \
    && git config --system --add safe.directory '*' \
    && rm -rf /var/lib/apt/lists/* /tmp/*

COPY --from=builder /src/target/release/retaskable /usr/local/bin/retaskable
COPY docker/agent/start.sh /usr/local/bin/start.sh
RUN chmod +x /usr/local/bin/start.sh

USER node
WORKDIR /work
ENTRYPOINT ["tini", "--"]
CMD ["/usr/local/bin/start.sh"]
