FROM openeuler/openeuler:24.03-lts
COPY prepare-deps.sh /tmp/prepare-deps.sh
RUN bash /tmp/prepare-deps.sh && dnf clean all
