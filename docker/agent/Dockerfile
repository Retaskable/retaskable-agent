# docs/specs/docker.md: the agent in a Linux container. Built from the repo root:
#   docker compose -f docker/agent/compose.yaml up -d --build

ARG GLAB_VERSION=1.51.0
ARG PLAYWRIGHT_VERSION=1.63.0
ARG AXE_CORE_VERSION=4.10.3
ARG CLAUDE_CODE_VERSION=2.1.312

# The agent's own executable, cross-compiled from source: this port has no release-bundle
# self-update yet (installers/03-the-bundle-agent-bundle.md section 3.12 isn't ported), so a newer
# agent means rebuilding this image, not something start.sh fetches.
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY prompts ./prompts
RUN cargo build --release --bin retaskable

# Node stays the base image: the Claude Code CLI the agent drives needs it, and so do the
# projects the agent builds and tests.
FROM node:22-bookworm-slim
ARG GLAB_VERSION
ARG PLAYWRIGHT_VERSION
ARG AXE_CORE_VERSION
ARG CLAUDE_CODE_VERSION

# Claude Code's own updater is off: its install here is root's, and a new one comes with a
# rebuild of this image.
ENV DISABLE_AUTOUPDATER=1 \
    CLAUDE_CONFIG_DIR=/home/node/.claude \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    RETASKABLE_A11Y_TOOLS=/opt/a11y-tools \
    RETASKABLE_A11Y_NO_SANDBOX=1 \
    RETASKABLE_DATA_DIR=/data \
    RETASKABLE_SECRETS_KEY_FILE=/data/secrets.key

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
    && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npx -y "playwright@${PLAYWRIGHT_VERSION}" install --with-deps chromium \
    && chmod -R a+rX "$PLAYWRIGHT_BROWSERS_PATH" \
    && mkdir -p "$RETASKABLE_A11Y_TOOLS" \
    && (cd "$RETASKABLE_A11Y_TOOLS" && npm init -y >/dev/null \
        && npm install --ignore-scripts --save-exact --no-audit --no-fund \
           "playwright-core@${PLAYWRIGHT_VERSION}" "axe-core@${AXE_CORE_VERSION}") \
    && chmod -R a+rX "$RETASKABLE_A11Y_TOOLS" \
    && git config --system --add safe.directory '*' \
    && rm -rf /var/lib/apt/lists/* /tmp/*

COPY --from=builder /src/target/release/retaskable /usr/local/bin/retaskable
COPY docker/agent/start.sh /usr/local/bin/start.sh
RUN chmod +x /usr/local/bin/start.sh

# A fresh named volume mounts over these as root; give them to `node` before switching to it, or
# the data folder (and the secrets key in it), Claude Code's sign-in/settings and gh's config can't be written.
RUN mkdir -p /work /data /home/node/.claude /home/node/.config/gh \
    && chown -R node:node /work /data /home/node

USER node
WORKDIR /work
ENTRYPOINT ["tini", "--"]
CMD ["/usr/local/bin/start.sh"]
