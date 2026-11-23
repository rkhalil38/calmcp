#!/usr/bin/env bash
# Creates a couple of test calendars (with one event) for the "test" user.
# Safe to re-run: existing calendars are left alone.
set -euo pipefail

BASE="${CALDAV_URL:-http://localhost:5232}"
AUTH="${CALDAV_USERNAME:-test}:${CALDAV_PASSWORD:-test}"

mkcalendar() {
    local slug="$1" name="$2" color="$3"
    local status
    status=$(curl -s -o /dev/null -w '%{http_code}' -u "$AUTH" -X MKCOL \
        -H 'Content-Type: application/xml' \
        "$BASE/${AUTH%%:*}/$slug/" --data-binary @- <<XML
<?xml version="1.0" encoding="utf-8"?>
<D:mkcol xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:I="http://apple.com/ns/ical/">
  <D:set><D:prop>
    <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
    <D:displayname>$name</D:displayname>
    <I:calendar-color>$color</I:calendar-color>
    <C:supported-calendar-component-set><C:comp name="VEVENT"/><C:comp name="VTODO"/></C:supported-calendar-component-set>
  </D:prop></D:set>
</D:mkcol>
XML
)
    case "$status" in
        201) echo "created  $slug" ;;
        405|409) echo "exists   $slug" ;;
        *) echo "failed   $slug (HTTP $status)" >&2; exit 1 ;;
    esac
}

mkcalendar personal "Personal" "#3a86ffff"
mkcalendar work "Work" "#ff006eff"

start=$(date -u +%Y%m%dT100000Z)
end=$(date -u +%Y%m%dT110000Z)
curl -sf -o /dev/null -u "$AUTH" -X PUT -H 'Content-Type: text/calendar' \
    "$BASE/${AUTH%%:*}/work/standup.ics" --data-binary @- <<ICS
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//calmcp//seed//EN
BEGIN:VEVENT
UID:standup@calmcp.test
DTSTAMP:$start
DTSTART:$start
DTEND:$end
SUMMARY:Standup
END:VEVENT
END:VCALENDAR
ICS
echo "seeded   work/standup.ics"
