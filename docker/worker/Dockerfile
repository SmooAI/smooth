FROM node:24-slim

# Install OpenCode and dependencies
RUN apt-get update && \
    apt-get install -y git ripgrep curl && \
    rm -rf /var/lib/apt/lists/* && \
    npm install -g opencode @smooth/tools

# Workspace mount point (target repo gets mounted here)
WORKDIR /workspace

# MCP server socket
ENV SMOOTH_MCP_SOCKET=/tmp/smooth-tools.sock

# OpenCode server mode port
EXPOSE 4096

# Start OpenCode in server mode
# The OPENCODE_CONFIG env var contains the JSON config
# which is written to .opencode.json on startup
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
