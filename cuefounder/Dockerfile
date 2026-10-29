FROM ghcr.io/block/buzz@sha256:ccde9f4d45f98914c912b71956f1c281af7d6d68ebdb30105ea051d98e0e4513
USER root
COPY LICENSE /usr/share/doc/cuefounder-buzz/LICENSE
COPY brand.css /srv/buzz/web/assets/cuefounder.css
COPY mark.svg /srv/buzz/web/assets/cuefounder-mark.svg
RUN sed -i 's#<title>Buzz</title>#<title>CueFounder Community</title>#; s#</head>#<link rel="icon" href="/assets/cuefounder-mark.svg" /><link rel="stylesheet" href="/assets/cuefounder.css" /></head>#; s#</body>#<footer class="cuefounder-attribution">CueFounder Community · A self-hosted fork of <a href="https://github.com/kravi4/cuefounder-buzz">Buzz</a>. Built on open source, with community updates to come.</footer></body>#' /srv/buzz/web/index.html
LABEL org.opencontainers.image.title="CueFounder Buzz" \
      org.opencontainers.image.source="https://github.com/kravi4/cuefounder-buzz" \
      org.opencontainers.image.version="2026.10.28.1" \
      org.opencontainers.image.licenses="Apache-2.0"
USER buzz
