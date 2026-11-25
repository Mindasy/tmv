CC ?= cc
CFLAGS ?= -O2 -g -Wall -Wextra -Werror

all: libtmvtrace.so
libtmvtrace.so: tmv_trace.c
	$(CC) $(CFLAGS) -fPIC -shared -fno-builtin -o $@ $< -pthread
clean:
	rm -f libtmvtrace.so fixture
.PHONY: all clean
