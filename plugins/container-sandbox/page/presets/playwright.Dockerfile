FROM docker.io/library/node:24-bookworm
# Outside HOME, which is empty at every run, and readable by the run's user, which is the server's.
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
RUN npx -y playwright install --with-deps chromium \
 && chmod -R a+rX /ms-playwright
