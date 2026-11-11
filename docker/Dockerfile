FROM ubuntu:26.04
RUN apt-get update && apt-get install -y ca-certificates ipset iptables && rm -rf /var/lib/apt/lists/*
COPY hashiverse-server /usr/local/bin/hashiverse-server
VOLUME /data
EXPOSE 443
# The server shuts down gracefully on SIGINT (Ctrl+C). It doesn't handle Docker's default SIGTERM,
# which PID 1 then ignores, so every `docker stop` and auto-update would end in a SIGKILL 10s later.
# Same as KillSignal= in packaging/systemd/hashiverse-server.service.
STOPSIGNAL SIGINT
CMD ["hashiverse-server", "--base-path", "/data"]
