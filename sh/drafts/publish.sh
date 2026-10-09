#!/usr/bin/env bash
# Submit a new draft version to the IETF datatracker: sh/drafts/publish.sh NAME VERSION EMAIL
#
# The datatracker emails the submitter a confirmation link; the submission is
# not final until that link is clicked. For a brand-new draft (-00) set
# "Replaces" on the confirmation page.
#
# A 200 or 201 also opens an empty changelog section for the next version,
# above the one just published and in that heading's style. The XML is built
# first, so the submitted text does not contain the new section. Commit the
# source edit; that commit is the record of the publish. Anything short of a
# 200 or 201 leaves the file untouched.
set -euo pipefail

usage="usage: sh/drafts/publish.sh NAME VERSION EMAIL"

# open_next_section FILE PUBLISHED NEXT [--write]
#
# FILE must already have one changelog heading for PUBLISHED. --write inserts
# an empty NEXT section directly above it, copying that heading's style,
# including a following {:...} attribute line. An existing NEXT heading above
# PUBLISHED is left alone. Existing entries are never moved or rewritten.
open_next_section() {
    local file=$1
    local published=$2
    local next=$3
    local write=0
    if [[ ${4:-} == --write ]]; then
        write=1
    elif [[ -n ${4:-} ]]; then
        echo "open_next_section: unknown argument ${4}" >&2
        return 2
    fi

    local -a lines=()
    mapfile -t lines <"$file"
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

    local published_re="^## .*-${published}( \\(in progress\\))?$"
    local next_re="^## .*-${next}( \\(in progress\\))?$"
    local -i published_at=-1
    local -i next_at=-1
    local -i published_count=0
    local -i next_count=0
    local line
    for ((i = changelog + 1; i < n; i++)); do
        line=${lines[$i]}
        # The next top-level section ends the changelog appendix.
        if [[ $line =~ ^#[[:space:]] ]]; then
            break
        fi
        if [[ $line =~ $published_re ]]; then
            published_at=$i
            published_count+=1
        fi
        if [[ $line =~ $next_re ]]; then
            next_at=$i
            next_count+=1
        fi
    done

    if ((published_count != 1)); then
        echo "$file changelog has $published_count sections for $published, want 1" >&2
        return 1
    fi
    if ((next_count > 1)); then
        echo "$file changelog has $next_count sections for $next" >&2
        return 1
    fi
    if ((next_count == 1)); then
        if ((next_at > published_at)); then
            echo "$file changelog section $next is below $published" >&2
            return 1
        fi
        return 0
    fi

    local shape_re="^(## .*-)${published}( \\(in progress\\))?$"
    if [[ ! ${lines[$published_at]} =~ $shape_re ]]; then
        echo "$file heading style is not recognized: ${lines[$published_at]}" >&2
        return 1
    fi
    local heading="${BASH_REMATCH[1]}${next}${BASH_REMATCH[2]}"

    local -a attrs=()
    local -i j=$((published_at + 1))
    while ((j < n)) && [[ ${lines[$j]} =~ ^\{: ]]; do
        attrs+=("${lines[$j]}")
        j+=1
    done

    if ((write == 0)); then
        return 0
    fi

    local tmp
    tmp=$(mktemp)
    {
        for ((i = 0; i < published_at; i++)); do
            printf '%s\n' "${lines[$i]}"
        done
        printf '%s\n' "$heading"
        if ((${#attrs[@]} > 0)); then
            local attr
            for attr in "${attrs[@]}"; do
                printf '%s\n' "$attr"
            done
        fi
        printf '\n'
        for ((i = published_at; i < n; i++)); do
            printf '%s\n' "${lines[$i]}"
        done
    } >"$tmp"
    cat "$tmp" >"$file"
    rm -f "$tmp"
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
sed "s/$name-latest/$doc/g" "$name.md" | kramdown-rfc --v3 >"$doc.xml"
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
        echo "Changelog section $next is open in $name.md."
        ;;
    *)
        echo "Submission failed." >&2
        exit 1
        ;;
esac
