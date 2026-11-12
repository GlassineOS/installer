# SPDX-License-Identifier: GPL-2.0-or-later
# The web screens' test image: Alpine with a pinned Chromium (the version GlassineOS/ui pins for
# its boards, tools/chromium.pin) and node. make docker-web-check builds and uses it.
FROM alpine:3.24
ARG CHROMIUM=156.0.8078.79-r0
RUN apk add --no-cache "chromium=$CHROMIUM" nodejs npm font-dejavu make
