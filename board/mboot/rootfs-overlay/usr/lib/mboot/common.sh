#!/bin/sh

MBOOT_LOG_DIR=${MBOOT_LOG_DIR:-/var/log/mboot}

mboot_timestamp() { date '+%Y-%m-%dT%H:%M:%S%z'; }

mboot_log() {
	level=$1
	shift
	printf '%s [%s] %s\n' "$(mboot_timestamp)" "$level" "$*"
}

mboot_pid_is() {
	pid=$1
	needle=$2
	case "$pid" in ''|*[!0-9]*) return 1;; esac
	[ -r "/proc/$pid/cmdline" ] || return 1
	tr '\000' ' ' < "/proc/$pid/cmdline" | grep -Fq "$needle"
}

mboot_show_error() {
	code=$1
	shift
	message=$*
	mboot_log ERROR "$code: $message"
	if [ -w /dev/tty1 ]; then
		chvt 1 2>/dev/null || true
		{
			printf '\033[2J\033[H'
			printf 'mBoot could not start mochiOS\n\n'
			printf '%s\n\n' "$message"
			printf 'Error: %s\n' "$code"
			printf 'Logs: %s\n' "$MBOOT_LOG_DIR"
		} > /dev/tty1
	fi
}
