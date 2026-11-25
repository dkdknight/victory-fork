# usage: auto6.sh <log> <exe> -- as auto5.sh, for logs with colour codes (plain.py strips them first); run from the folder that holds plain.py and joinlog.py
log="$1"; exe="$2"; j="${log%.txt}-joined.txt"
for i in $(seq 1 300); do
  sleep 8
  python -I plain.py "$log" "${log%.txt}-plain.log"; python -I joinlog.py "${log%.txt}-plain.log" "$j" 2>/dev/null
  ok=$(grep -a "ms/frame" "$j" | tail -5 | grep " [4-9][0-9]\{3\} draws/frame" | grep -c "(\([0-9]\|[0-9][0-9]\|1[0-2][0-9]\)\.[0-9] preparing")
  if [ "$ok" -ge 5 ]; then
    sleep 15; python -I plain.py "$log" "${log%.txt}-plain.log"; python -I joinlog.py "${log%.txt}-plain.log" "$j"
    grep -a "ms/frame" "$j" | tail -6 | grep -o "^.\{19\}\|[0-9.]* fps, [0-9.]* ms/frame ([0-9.]* preparing\|; [0-9.]* on the GPU; [0-9.]* copying back\|[0-9]* draws/frame" | paste - - - - | cut -c12-
    grep -a "the card's clock" "$j" | tail -4 | grep -o "the card about [0-9]*% busy\|stood waiting for the card [0-9.]* ms" | paste - -
    grep -a "of the waiting for the card in settles" "$j" | tail -2 | grep -o "title memory read [0-9.]*, a step of ours [0-9.]*, lists in the air [0-9.]*"
    grep -a "ms/frame" "$j" | tail -1 | grep -o "preparing: [^;]*"
    grep -a -o "waits=[0-9]* amplified=[0-9]*" "$j" | tail -1
    exit 0
  fi
  if ! tasklist //FI "IMAGENAME eq $exe" 2>/dev/null | grep -qi "${exe%.exe}"; then echo EXITED; grep -a -i "device removed\|first-chance exception" "$j" | tail -2 | cut -c1-240; exit 0; fi
done
echo TIMEOUT
