#!/usr/bin/env bash
# Render the vertical demo reel with octo-render, then caption and encode it:
#
#   renders/demo/octo-render-demo.mp4   1080x1920, 30 fps, for socials
#   renders/demo/octo-render-demo.gif   small and looping, for the README
#
#   scripts/make-demo.sh              full render (a few minutes on 4 cores)
#   scripts/make-demo.sh --preview    270x480 quick look in target/demo-preview/
#
# Extra arguments go to the demo example (examples/demo.rs), e.g. --threads 2.
# Captions use a bold system font; set DEMO_FONT=/path/to/font.ttf to choose.
set -euo pipefail
cd "$(dirname "$0")/.."

preview=0
for arg in "$@"; do
    if [[ "$arg" == "--preview" ]]; then preview=1; fi
done
if ((preview)); then
    work=target/demo-preview
    dest=target/demo-preview
else
    work=target/demo
    dest=renders/demo
fi

fail() {
    echo "make-demo: $*" >&2
    exit 1
}

command -v ffmpeg >/dev/null || fail "ffmpeg is required (with libx264 and drawtext)"
# Capture the listings first: with pipefail, `grep -q` closing the pipe early
# would make ffmpeg's SIGPIPE look like a missing feature.
filters=$(ffmpeg -hide_banner -filters 2>/dev/null || true)
encoders=$(ffmpeg -hide_banner -encoders 2>/dev/null || true)
grep -q " drawtext " <<<"$filters" ||
    fail "this ffmpeg has no drawtext filter (it needs libfreetype)"
grep -q " libx264 " <<<"$encoders" ||
    fail "this ffmpeg has no libx264 encoder"

find_font() {
    if [[ -n "${DEMO_FONT:-}" ]]; then
        [[ -f "$DEMO_FONT" ]] || fail "DEMO_FONT '$DEMO_FONT' does not exist"
        echo "$DEMO_FONT"
        return
    fi
    if command -v fc-match >/dev/null; then
        for pattern in "Inter Display:black" "Inter:black" "Inter:extrabold" "sans-serif:bold"; do
            local file
            file=$(fc-match -f '%{file}' "$pattern" 2>/dev/null || true)
            if [[ -f "$file" ]]; then
                echo "$file"
                return
            fi
        done
    fi
    for file in \
        /usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf \
        /Library/Fonts/Arial\ Bold.ttf \
        /System/Library/Fonts/Supplemental/Arial\ Bold.ttf; do
        if [[ -f "$file" ]]; then
            echo "$file"
            return
        fi
    done
    fail "no bold font found; set DEMO_FONT=/path/to/font.ttf"
}
font=$(find_font)
echo "make-demo: captions in $font"

cargo run --release --example demo -- --out "$work" "$@"

# WIDTH HEIGHT FPS FRAMES DURATION END_CARD_START
# shellcheck disable=SC1091
source "$work/meta.env"

# Sizes scale with the frame height so the preview matches the full render.
px() { awk -v h="$HEIGHT" -v f="$1" 'BEGIN { printf "%d", h * f + 0.5 }'; }
caption_size=$(px 0.044)
caption_border=$(px 0.0045)
caption_spacing=$(px 0.006)
bar_height=$(px 0.005)
title_size=$(px 0.078)
tagline_size=$(px 0.030)
url_size=$(px 0.020)

# End-card lines are text files too, so nothing needs filter escaping.
printf 'octo-render' >"$work/captions/end_title.txt"
printf '100%% CPU · from scratch in Rust' >"$work/captions/end_tagline.txt"
printf 'github.com/MysterionRise/octo-render' >"$work/captions/end_url.txt"

text() {
    # text FILE SIZE Y_EXPR START END
    local file=$1 size=$2 y=$3 start=$4 end=$5
    printf "drawtext=fontfile='%s':textfile='%s':expansion=none:fontcolor=white" "$font" "$file"
    printf ":fontsize='%s*(1+0.22*max(0,1-(t-%s)/0.14))'" "$size" "$start"
    printf ":borderw=%s:bordercolor=black@0.9:line_spacing=%s:text_align=C" "$caption_border" "$caption_spacing"
    printf ":x=(w-text_w)/2:y='%s'" "$y"
    printf ":alpha='clip((t-%s)/0.08,0,1)*clip((%s-t)/0.1,0,1)'" "$start" "$end"
    printf ":enable='between(t,%s,%s)'" "$start" "$end"
}

graph="$work/filtergraph.txt"
{
    printf '[0:v]format=rgb24'
    while IFS=$'\t' read -r start end file; do
        printf ',%s' "$(text "$work/$file" "$caption_size" 'h*0.135-text_h/2' "$start" "$end")"
    done <"$work/timeline.tsv"
    # Progress bar: a dim track with a white bar sliding in from the left.
    printf ',drawbox=x=0:y=0:w=iw:h=%s:color=black@0.45:t=fill[base];\n' "$bar_height"
    printf 'color=c=white:s=%sx%s:r=%s[bar];\n' "$WIDTH" "$bar_height" "$FPS"
    printf "[base][bar]overlay=x='-w+w*min(1,(t+1/%s)/%s)':y=0:shortest=1[barred];\n" "$FPS" "$DURATION"
    # End card: dim the scene, then title, tagline, and URL.
    printf 'color=c=black@0.62:s=%sx%s:r=%s,format=rgba,fade=t=in:st=%s:d=0.5:alpha=1[dim];\n' \
        "$WIDTH" "$HEIGHT" "$FPS" "$END_CARD_START"
    printf '[barred][dim]overlay=shortest=1'
    printf ',%s' "$(text "$work/captions/end_title.txt" "$title_size" 'h*0.40-text_h/2' "$END_CARD_START" 999)"
    printf ',%s' "$(text "$work/captions/end_tagline.txt" "$tagline_size" 'h*0.47-text_h/2' "$END_CARD_START" 999)"
    printf ',%s' "$(text "$work/captions/end_url.txt" "$url_size" 'h*0.52-text_h/2' "$END_CARD_START" 999)"
    printf ',scale=out_color_matrix=bt709,format=yuv420p[out]\n'
} >"$graph"

mkdir -p "$dest"
mp4="$dest/octo-render-demo.mp4"
gif="$dest/octo-render-demo.gif"
ffmpeg -hide_banner -loglevel error -y \
    -framerate "$FPS" -i "$work/frames/frame_%05d.png" \
    -filter_complex_script "$graph" -map '[out]' -frames:v "$FRAMES" \
    -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p \
    -colorspace bt709 -color_primaries bt709 -color_trc bt709 \
    -movflags +faststart "$mp4"

bytes() { wc -c <"$1" | tr -d ' '; }
mb() { awk -v b="$1" 'BEGIN { printf "%.1f MB", b / 1048576 }'; }
mp4_bytes=$(bytes "$mp4")
echo "make-demo: $mp4 ($(mb "$mp4_bytes"))"
if ((!preview && mp4_bytes > 15 * 1048576)); then
    echo "make-demo: warning: the MP4 is over 15 MB" >&2
fi

# GIF: the largest size and frame rate that fits the README budget.
gif_budget=$((8 * 1048576))
for setting in "360 15" "320 12" "270 12"; do
    read -r gif_width gif_fps <<<"$setting"
    if ((preview)); then gif_width=$WIDTH; fi
    ffmpeg -hide_banner -loglevel error -y -i "$mp4" -vf \
        "fps=$gif_fps,scale=$gif_width:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=256:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
        -loop 0 "$gif"
    gif_bytes=$(bytes "$gif")
    echo "make-demo: $gif at ${gif_width}px, ${gif_fps} fps ($(mb "$gif_bytes"))"
    if ((preview || gif_bytes <= gif_budget)); then
        exit 0
    fi
done
fail "the GIF is over 8 MB even at 270px and 12 fps"
