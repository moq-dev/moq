#!/usr/bin/env bash
# Submit a new draft version to the IETF datatracker: sh/drafts/publish.sh NAME VERSION EMAIL
#
# The datatracker emails the submitter a confirmation link; the submission is
# not final until that link is clicked. For a brand-new draft (-00) set
# "Replaces" on the confirmation page.
#
# A 200 or 201 opens an empty changelog section above the one just published.
# A versioned heading such as `## moq-lite-07` stays as it is, and the new
# heading ends in the next version. A `## Since <name>-<prev> (in progress)`
# heading is the unpublished section for version prev+1: publishing it drops
# ` (in progress)` and opens `## Since <name>-<version> (in progress)` above
# it, copying a following {:...} line. Bullets are not rewritten.
#
# The XML is built from the source with an already-opened next section
# omitted, including when publish is run again after that section exists.
# Anything short of a 200 or 201 leaves the file untouched. The update is an
# atomic rename beside the draft; a failed write leaves the original file.
set -euo pipefail

usage="usage: sh/drafts/publish.sh NAME VERSION EMAIL"

# open_next_section FILE PUBLISHED NEXT [--write|--render]
#
# FILE must have one changelog heading for PUBLISHED. Versioned headings end
# in PUBLISHED. A Since heading ends in the previous version and may say
# `(in progress)`. --write inserts the empty next section when it is absent.
# --render prints the markdown to submit, without that next section.
open_next_section() {
    local file=$1
    local published=$2
    local next=$3
    local mode="check"
    case ${4:-} in
        "") mode="check" ;;
        --write) mode="write" ;;
        --render) mode="render" ;;
        *)
            echo "open_next_section: unknown argument ${4}" >&2
            return 2
            ;;
    esac

    local -a lines=()
    mapfile -t lines <"$file" || return 1
    local -i n=${#lines[@]}
    local -i changelog=-1
    local -i i=0
    for ((i = 0; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#\ .*Changelog[[:space:]]*$ ]]; then
            changelog=$i
            break
        fi
    done
    if ((changelog < 0)); then
        echo "$file has no changelog section for $published" >&2
        return 1
    fi

    local -i version_num=$((10#$published))
    local prev=""
    if ((version_num > 0)); then
        prev=$(printf '%02d' $((version_num - 1)))
    fi

    local -i end=$n
    local -i since_count=0
    local -i versioned_count=0
    local since_re='^## Since .+-[0-9][0-9]( \(in progress\))?$'
    local versioned_re='^## .*-([0-9][0-9])( \(in progress\))?$'
    for ((i = changelog + 1; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#[[:space:]] ]]; then
            end=$i
            break
        fi
        if [[ ${lines[$i]} =~ $since_re ]]; then
            since_count+=1
        elif [[ ${lines[$i]} =~ $versioned_re ]]; then
            versioned_count+=1
        fi
    done
    if ((since_count > 0 && versioned_count > 0)); then
        echo "$file changelog mixes Since and versioned headings" >&2
        return 1
    fi

    local -i published_at=-1
    local -i next_at=-1
    local -i published_count=0
    local -i next_count=0
    local style=versioned
    local line
    if ((since_count > 0)); then
        style=since
        # Since-<prev> (in progress) is the section being published as
        # prev+1. Since-<published> (in progress) is the section a previous
        # successful publish already opened.
        local open_re=""
        local closed_re=""
        local next_re="^## Since (.+)-${published} \\(in progress\\)$"
        if [[ -n $prev ]]; then
            open_re="^## Since (.+)-${prev} \\(in progress\\)$"
            closed_re="^## Since (.+)-${prev}$"
        fi
        for ((i = changelog + 1; i < end; i++)); do
            line=${lines[$i]}
            if [[ -n $open_re && $line =~ $open_re ]]; then
                published_at=$i
                published_count+=1
            elif [[ -n $closed_re && $line =~ $closed_re ]]; then
                published_at=$i
                published_count+=1
            elif [[ $line =~ $next_re ]]; then
                next_at=$i
                next_count+=1
            fi
        done
    else
        local published_re="^## .*-${published}( \\(in progress\\))?$"
        local next_heading_re="^## .*-${next}( \\(in progress\\))?$"
        for ((i = changelog + 1; i < end; i++)); do
            line=${lines[$i]}
            if [[ $line =~ $published_re ]]; then
                published_at=$i
                published_count+=1
            fi
            if [[ $line =~ $next_heading_re ]]; then
                next_at=$i
                next_count+=1
            fi
        done
    fi

    if ((published_count != 1)); then
        echo "$file changelog has $published_count sections for $published, want 1" >&2
        return 1
    fi
    if ((next_count > 1)); then
        echo "$file changelog has $next_count next sections" >&2
        return 1
    fi
    if ((next_count == 1 && next_at > published_at)); then
        echo "$file changelog next section is below the one for $published" >&2
        return 1
    fi

    if [[ $mode == render ]]; then
        if ((next_count == 0)); then
            cat "$file" || return 1
            return 0
        fi
        for ((i = 0; i < next_at; i++)); do
            printf '%s\n' "${lines[$i]}"
        done
        for ((i = published_at; i < n; i++)); do
            printf '%s\n' "${lines[$i]}"
        done
        return 0
    fi

    if ((next_count == 1)) || [[ $mode == check ]]; then
        return 0
    fi

    local heading
    local published_out=${lines[$published_at]}
    if [[ $style == since ]]; then
        local since_shape='^## Since (.+)-[0-9][0-9]( \(in progress\))?$'
        if [[ ! $published_out =~ $since_shape ]]; then
            echo "$file heading style is not recognized: $published_out" >&2
            return 1
        fi
        heading="## Since ${BASH_REMATCH[1]}-${published} (in progress)"
        if [[ $published_out == *" (in progress)" ]]; then
            published_out=${published_out%" (in progress)"}
        fi
    else
        local shape_re="^(## .*-)${published}( \\(in progress\\))?$"
        if [[ ! $published_out =~ $shape_re ]]; then
            echo "$file heading style is not recognized: $published_out" >&2
            return 1
        fi
        heading="${BASH_REMATCH[1]}${next}${BASH_REMATCH[2]}"
    fi

    local -a attrs=()
    local -i j=$((published_at + 1))
    while ((j < n)) && [[ ${lines[$j]} =~ ^\{: ]]; do
        attrs+=("${lines[$j]}")
        j+=1
    done

    # Called as `if ! open_next_section`, which disables errexit for this
    # whole function. A failed mktemp or redirect must not fall through.
    local tmp
    tmp=$(mktemp "${file}.tmp.XXXXXX") || return 1
    local write_failed=0
    local attr
    local -i w
    {
        for ((w = 0; w < published_at; w++)); do
            printf '%s\n' "${lines[$w]}" || write_failed=1
        done
        printf '%s\n' "$heading" || write_failed=1
        if ((${#attrs[@]} > 0)); then
            for attr in "${attrs[@]}"; do
                printf '%s\n' "$attr" || write_failed=1
            done
        fi
        printf '\n' || write_failed=1
        printf '%s\n' "$published_out" || write_failed=1
        for ((w = published_at + 1; w < n; w++)); do
            printf '%s\n' "${lines[$w]}" || write_failed=1
        done
        ((write_failed == 0))
    } >"$tmp" || write_failed=1
    if ((write_failed != 0)); then
        rm -f -- "$tmp"
        return 1
    fi
    if ! chmod --reference="$file" "$tmp"; then
        rm -f -- "$tmp"
        return 1
    fi
    if ! mv -f -- "$tmp" "$file"; then
        rm -f -- "$tmp"
        return 1
    fi
}

name=${1:?$usage}
version=${2:?$usage}
email=${3:?$usage}

cd "$(git rev-parse --show-toplevel)/drafts"
case "$version" in
    [0-9][0-9]) ;;
    *)
        echo "version must be two digits, e.g. 05" >&2
        exit 1
        ;;
esac

next=$((10#$version + 1))
if ((next > 99)); then
    echo "version $version has no two-digit successor" >&2
    exit 1
fi
next=$(printf '%02d' "$next")

doc="$name-$version"
if [[ ! -f "$name.md" ]]; then
    echo "no such draft: $name.md" >&2
    exit 1
fi

# A style we cannot extend fails here, before the datatracker has the XML.
open_next_section "$name.md" "$version" "$next"

echo "Building $doc.xml"
open_next_section "$name.md" "$version" "$next" --render | sed "s/$name-latest/$doc/g" | kramdown-rfc --v3 >"$doc.xml"
echo "Submitting $doc.xml to the datatracker as $email"
resp="$(mktemp)"
code="$(curl -sS -o "$resp" -w '%{http_code}' \
    -F "user=$email" -F "xml=@$doc.xml" \
    https://datatracker.ietf.org/api/submission)"
echo "HTTP $code"
cat "$resp"
echo
rm -f "$resp"
case "$code" in
    200 | 201)
        if ! open_next_section "$name.md" "$version" "$next" --write; then
            echo "The datatracker accepted $doc, but $name.md was not updated." >&2
            exit 1
        fi
        echo "Submitted. Check $email for the confirmation link."
        echo "Changelog section for the next version is open in $name.md."
        ;;
    *)
        echo "Submission failed." >&2
        exit 1
        ;;
esac
