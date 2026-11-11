FROM ubuntu:26.04
RUN apt-get update && apt-get install -y ca-certificates ipset iptables && rm -rf /var/lib/apt/lists/*
COPY hashiverse-server /usr/local/bin/hashiverse-server
VOLUME /data
EXPOSE 443
CMD ["hashiverse-server", "--base-path", "/data"]
