#!/bin/zsh
# One measurement in a fresh Chrome app window (server.py must be running):
#   run.sh <host ip> <label> <ours|zremote> <query>
# e.g. run.sh 10.1.77.158 auto ours "at=decode&codecstr=hev1.1.6.L186.B0&req=host%3Dwin-dev%26fps%3D100%26edge%3D3840%26kbps%3D40000%26codecs%3Dhevc"
#      run.sh 10.1.77.158 zr zremote "at=decode&whep=http://10.1.77.158:8889/screen/whep"
here=${0:A:h}; ip=$1; label=$2; mode=$3; extra=$4
off=$(python3 $here/offset.py $ip | cut -d' ' -f1)
url="http://127.0.0.1:18780/probe.html?mode=$mode&label=$label&offset=$off&secs=20&$extra"
dir=$(mktemp -d /tmp/g2g-chrome.XXXX)
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --user-data-dir=$dir --no-first-run --no-default-browser-check \
  --disable-backgrounding-occluded-windows --disable-renderer-backgrounding --disable-background-timer-throttling \
  --autoplay-policy=no-user-gesture-required --window-size=700,500 --window-position=60,60 --app="$url" >/dev/null 2>&1 &
sleep 30
pkill -f "user-data-dir=$dir"; sleep 1; rm -rf $dir
echo "offset $off"
tail -1 $here/results.jsonl
