FROM node:24-slim

# Install bd CLI and Docker CLI for Smooth Operator management
RUN apt-get update && \
    apt-get install -y docker.io curl && \
    rm -rf /var/lib/apt/lists/* && \
    npm install -g bd

WORKDIR /app

# Copy built leader package
COPY packages/leader/dist ./dist
COPY packages/leader/package.json ./
COPY node_modules ./node_modules

EXPOSE 4400

ENV NODE_ENV=production
ENV PORT=4400

CMD ["node", "dist/server.js"]
