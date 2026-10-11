#!/bin/bash
# Inventario de la shell de macOS: ejecuta órdenes típicas dentro del bash de macOS (unido a launchd) y resume cuáles
# funcionan. Cada orden tiene un plazo (perl alarm, porque macOS no trae `timeout`) y se clasifica:
#   OK      termina con código 0
#   ERR n   termina con código n (se muestra la primera línea de su salida)
#   COLGADA no termina en el plazo
#   SEÑAL   la mata una señal (caída)
#
#   tools/shell_survey.sh                 # lista por omisión
#   tools/shell_survey.sh 'orden 1' 'orden 2' ...
#
# Usa tools/macos-shell.sh --launchd (arranca launchd y lo detiene al terminar). PLAZO: segundos por orden (6).
TOOLS="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
PLAZO="${PLAZO:-6}"
if [ $# -eq 0 ]; then
	set -- 'sw_vers' 'uname -a' 'id' 'whoami' 'hostname' 'date' 'uptime' 'ls -la /' 'ps ax' 'top -l 1 -n 5' \
		'df -h' 'mount' 'du -sh /usr/bin' 'stat /' 'file /bin/ls' 'which ls' 'env' 'ulimit -a' \
		'sysctl hw.ncpu hw.memsize kern.osversion' 'vm_stat' 'memory_pressure -Q' 'system_profiler SPSoftwareDataType' \
		'ifconfig' 'netstat -rn' 'ping -c 1 127.0.0.1' 'scutil --get ComputerName' 'networksetup -listallnetworkservices' \
		'launchctl list' 'launchctl print system' 'defaults read -g' 'defaults write com.tahoe.survey k v' \
		'plutil -p /System/Library/CoreServices/SystemVersion.plist' 'diskutil list' 'diskutil info /' 'hdiutil info' \
		'csrutil status' 'nvram -p' 'ioreg -l -w0 -d2' 'pmset -g' 'log show --last 1m' 'dscl . -list /Users' \
		'dscacheutil -q user -a name root' 'security list-keychains' 'codesign -dv /bin/ls' 'spctl --status' \
		'tar -cf /tmp/s.tar /etc/hosts' 'gzip -c /etc/hosts' 'zip -q /tmp/s.zip /etc/hosts' 'perl -e "print 1"' \
		'python3 -c "print(1)"' 'ruby -e "puts 1"' 'vim --version' 'less --version' 'grep --version' 'awk "BEGIN{print 1}"' \
		'sed --version' 'find /etc -maxdepth 1' 'xargs --version' 'sort /etc/hosts' 'curl -V' 'ssh -V' \
		'kill -0 $$' 'sleep 1' 'touch /tmp/s && rm /tmp/s' 'mkdir -p /tmp/s/d && rm -r /tmp/s' \
		'ln -s /tmp /tmp/sl && rm /tmp/sl' 'chmod 600 /etc/hosts' 'chown root /etc/hosts' 'xattr -l /etc/hosts' \
		'open -h' 'osascript -e "return 1"' 'say -v ?' 'caffeinate -t 1' 'shutdown -h +60 && shutdown -c' \
		'softwareupdate -l' 'installer -help' 'startosinstall --usage' 'resetpassword -h' 'bless --info'
fi
SCRIPT="t() { c=\"\$1\"; f=/tmp/.survey.\$\$; /bin/bash -c \"\$c\" > \$f 2>&1 < /dev/null & p=\$!; i=0
 while kill -0 \$p 2>/dev/null && [ \$i -lt $((PLAZO * 10)) ]; do sleep 0.1; i=\$((i+1)); done
 if kill -0 \$p 2>/dev/null; then kill -9 \$p 2>/dev/null; wait \$p 2>/dev/null; r=999; else wait \$p; r=\$?; fi
 l=\$(head -1 \$f | cut -c1-90); rm -f \$f
 if [ \$r = 0 ]; then e=OK; elif [ \$r = 999 ]; then e=COLGADA; elif [ \$r -gt 128 ]; then e=\"SEÑAL \$((r-128))\"; else e=\"ERR \$r\"; fi
 printf '%-9s %-40s %s\\n' \"\$e\" \"\$c\" \"\$( [ \$r = 0 ] || echo \"\$l\")\"; }
"
for c in "$@"; do SCRIPT+="t $(printf '%q' "$c")
"; done
[ -n "$TAHOE_LOGFILE" ] && rm -f "$TAHOE_LOGFILE"          # un registro por inventario (tahoe-run añade al final)
"$TOOLS/macos-shell.sh" --launchd -c "$SCRIPT"
