# Day One Decrypted Entry Body Format

After a D1 blob (format version 2) is decrypted and gunzipped, the plaintext is a UTF-8 encoded JSON object representing a single journal entry. This document describes every field in that object.

See also:
- [`d1-binary-format.md`](d1-binary-format.md) — how the entry is encrypted on disk/wire
- [`rtjson-format.md`](rtjson-format.md) — the rich text format used by the `richTextJSON` field
- [`feature-flag-tool.html`](feature-flag-tool.html) — interactive bit inspector for `featureFlags`

---

## Top-level structure

```json
{
  "id": "A1B2C3D4E5F6...",
  "date": 1700000000000,
  "userEditDate": 1700000001000,
  "timeZone": "America/New_York",
  "isAllDay": false,
  "isPinned": false,
  "starred": false,
  "featureFlags": "13",
  "body": "Optional markdown body...",
  "richTextJSON": { "meta": { "version": 1 }, "nodes": [] },
  "tags": ["travel", "personal"],
  "activity": "Walking",
  "location": { ... },
  "weather": { ... },
  "steps": { "stepCount": 8432, "ignore": false },
  "music": { "artist": "...", "track": "...", "album": "...", "albumYear": 2023 },
  "moments": [ ... ],
  "clientMeta": { ... },
  "ownerUserId": "user-uuid",
  "creatorUserId": "user-uuid",
  "editorUserId": "user-uuid",
  "lastEditingDeviceID": "device-uuid",
  "lastEditingDeviceName": "iPhone 15",
  "editingTime": 42000,
  "duration": 0,
  "templateID": null,
  "promptID": null,
  "entryType": null,
  "unread_marker_id": null,
  "is_shared": false
}
```

---

## Field reference

### Identity and timestamps

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | `string` | Yes | UUID identifying this entry |
| `date` | `number` | Yes | Entry date as Unix milliseconds. This is the user-visible date, not necessarily when the entry was created. |
| `userEditDate` | `number \| null` | Yes | Timestamp (ms) of the last user-initiated edit. `null` if never edited after creation. |
| `editingTime` | `number` | No | Cumulative time the user spent editing, in milliseconds. Tracked by native apps only. |
| `duration` | `number` | No | Duration in milliseconds. Usage varies by entry type. |

### Content

| Field | Type | Required | Description |
|---|---|---|---|
| `body` | `string` | No | Markdown plaintext body. Present on older entries and those created without RTJ support. Absent when `richTextJSON` is the content source (i.e. when `featureFlags` bit `0x0010` is set). |
| `richTextJSON` | `RichTextJSON \| string \| {}` | No | Structured rich text content. See [`rtjson-format.md`](rtjson-format.md) for the full spec. May arrive as a JSON-encoded string rather than an inline object — clients must handle both. An empty object `{}` means no content. When present and non-empty, takes precedence over `body`. |

Only one of `body` or `richTextJSON` carries the canonical content at a given time. Prefer `richTextJSON` when `featureFlags` bit `0x0010` is set.

### Display and organization

| Field | Type | Required | Description |
|---|---|---|---|
| `timeZone` | `string \| null` | Yes | IANA time zone name (e.g. `"America/New_York"`, `"Europe/London"`). Used to display `date` in the correct local time. `null` means UTC. |
| `isAllDay` | `boolean` | Yes | If `true`, the entry represents a full day and the time component of `date` should be ignored in display. |
| `isPinned` | `boolean` | Yes | If `true`, the entry is pinned and floats at the top of the journal timeline. Also reflected in `featureFlags` bit `0x0200`. |
| `starred` | `boolean` | Yes | Whether the entry is starred/favorited. |
| `tags` | `string[] \| null` | Yes | List of tag strings. `null` or empty array means no tags. |
| `templateID` | `string \| null` | Yes | UUID of the template used to create this entry. `null` if not template-derived. |
| `promptID` | `string \| null` | Yes | UUID of the writing prompt associated with this entry. `null` if none. |
| `entryType` | `string` | No | Non-empty string for special entry types (e.g. `"welcome-entry"`). When present, `featureFlags` bit `0x0800` is set. |

### Activity and context

| Field | Type | Required | Description |
|---|---|---|---|
| `activity` | `string` | No | Detected physical activity at time of entry. One of: `"Stationary"`, `"Walking"`, `"Running"`, `"Biking"`, `"Eating"`, `"Automotive"`, `"Flying"`, `"Train"`, `""`. |
| `steps` | `EntrySteps \| null` | No | Step count data. See type below. |
| `music` | `EntryMusic \| null` | No | Currently playing music at time of entry. See type below. |

### Ownership

| Field | Type | Required | Description |
|---|---|---|---|
| `ownerUserId` | `string` | Yes | User ID of the journal owner. |
| `creatorUserId` | `string` | Yes | User ID who originally created the entry. |
| `editorUserId` | `string` | Yes | User ID who last saved this revision. |
| `lastEditingDeviceID` | `string` | No | Device UUID of the last editor. |
| `lastEditingDeviceName` | `string` | No | Human-readable name of the last editing device. |

### Feature detection

| Field | Type | Required | Description |
|---|---|---|---|
| `featureFlags` | `string` | Yes | Lowercase hexadecimal bit field indicating which features this entry uses. No `0x` prefix. Never empty — use `"0"` if no features are set. See [Feature Flags](#feature-flags) below. |

### Sync and sharing

| Field | Type | Required | Description |
|---|---|---|---|
| `unread_marker_id` | `string \| null` | Yes | ID used to track read/unread state in shared journals. `null` if not applicable. |
| `is_shared` | `boolean` | No | Whether this entry has been shared. |

---

## Nested types

### `location`

GPS and reverse-geocoded location at the time of the entry. `null` if no location was recorded.

```json
{
  "latitude": 40.7128,
  "longitude": -74.0060,
  "altitude": 10.0,
  "heading": 270.0,
  "speed": 1.4,
  "placeName": "Empire State Building",
  "localityName": "New York City",
  "administrativeArea": "New York",
  "country": "United States",
  "streetAddress": "350 5th Ave",
  "timeZoneName": "America/New_York",
  "fullAddress": "350 5th Ave, New York, NY 10118, United States",
  "userLabel": "Work",
  "region": {
    "identifier": "region-uuid",
    "latitude": 40.7128,
    "longitude": -74.0060,
    "radius": 100.0
  },
  "route": {
    "summary_polyline": ["encodedPolylineString"],
    "start_latlng": [40.7128, -74.0060],
    "end_latlng": [40.7580, -73.9855],
    "source": "Strava"
  }
}
```

| Field | Type | Notes |
|---|---|---|
| `latitude` | `number` | Decimal degrees WGS-84 |
| `longitude` | `number` | Decimal degrees WGS-84 |
| `altitude` | `number` | Meters above sea level |
| `heading` | `number` | Degrees from true north (0–360) |
| `speed` | `number` | Meters per second |
| `placeName` | `string` | Specific venue or landmark name |
| `localityName` | `string` | City or locality |
| `administrativeArea` | `string` | State, province, or region |
| `country` | `string` | Country name |
| `streetAddress` | `string` | Street-level address |
| `timeZoneName` | `string` | IANA time zone at this location |
| `fullAddress` | `string` | Optional full formatted address |
| `userLabel` | `string` | Optional user-assigned label. Deprecated — `featureFlags` bit `0x0004` is no longer meaningful. |
| `region` | `object` | Circular geofence region |
| `route` | `object` | Optional route data. Present when `featureFlags` bit `0x10000` is set. |

### `weather`

Weather conditions at the time and location of the entry. `null` if not recorded.

```json
{
  "description": "Partly Cloudy",
  "tempCelsius": 18.5,
  "code": "partly-cloudy-day",
  "moonPhase": 0.42,
  "moonPhaseCode": "waxing-gibbous",
  "service": "DarkSky",
  "windBearing": 225,
  "windSpeedKph": 14.8,
  "windChillCelsius": 16.2,
  "pressureMb": 1013.25,
  "visibilityKm": 16.0,
  "relativeHumidity": 62,
  "sunriseDate": 1700000000000,
  "sunsetDate": 1700043600000
}
```

| Field | Type | Notes |
|---|---|---|
| `description` | `string` | Human-readable condition string |
| `tempCelsius` | `number` | Temperature in degrees Celsius |
| `code` | `string` | Machine-readable weather condition code |
| `moonPhase` | `number` | Moon phase as 0.0–1.0 fraction of cycle |
| `moonPhaseCode` | `string` | Named moon phase (e.g. `"waxing-gibbous"`) |
| `service` | `string` | Weather data provider (e.g. `"DarkSky"`, `"WeatherKit"`) |
| `windBearing` | `number` | Wind direction in degrees |
| `windSpeedKph` | `number` | Wind speed in km/h |
| `windChillCelsius` | `number` | Wind chill in degrees Celsius |
| `pressureMb` | `number` | Barometric pressure in millibars |
| `visibilityKm` | `number` | Visibility in kilometers |
| `relativeHumidity` | `number` | Relative humidity as a percentage |
| `sunriseDate` | `number` | Sunrise time as Unix milliseconds |
| `sunsetDate` | `number` | Sunset time as Unix milliseconds |

### `steps`

Step count data for the entry date. `null` or absent if not recorded.

```json
{
  "stepCount": 8432,
  "ignore": false
}
```

| Field | Type | Notes |
|---|---|---|
| `stepCount` | `number` | Number of steps |
| `ignore` | `boolean` | If `true`, clients should not display this step count |

### `music`

Music playing at the time the entry was created. `null` or absent if not recorded.

```json
{
  "artist": "Radiohead",
  "track": "Karma Police",
  "album": "OK Computer",
  "albumYear": 1997
}
```

### `moments`

Array of attachment metadata objects. Actual binary content (image, audio, video, PDF) is stored separately and referenced by `id`. An entry with no attachments has an empty array.

```json
[
  {
    "id": "moment-uuid",
    "type": "photo",
    "contentType": "image/jpeg",
    "md5": "d41d8cd98f00b204e9800998ecf8427e",
    "date": 1700000000000,
    "createdAt": 1700000000000,
    "height": 3024,
    "width": 4032,
    "isSketch": false,
    "favorite": false,
    "creationDevice": "iPhone 15 Pro",
    "creationDeviceIdentifier": "device-uuid",
    "thumbnail": {
      "md5": "abc123",
      "contentType": "image/jpeg",
      "fileSize": 12345,
      "height": 256,
      "width": 341
    },
    "thumbnailContentType": "image/jpeg",
    "duration": null,
    "location": { ... },
    "title": "Optional caption"
  }
]
```

| Field | Type | Notes |
|---|---|---|
| `id` | `string` | UUID used to fetch the binary content |
| `type` | `string` | `"photo"`, `"audio"`, `"video"`, `"pdf"` |
| `contentType` | `string` | MIME type of the attachment |
| `md5` | `string` | MD5 hash of the binary content for integrity |
| `date` | `number` | Capture date as Unix milliseconds |
| `createdAt` | `number` | Upload/creation timestamp as Unix milliseconds |
| `height` | `number` | Pixel height (photos/video) |
| `width` | `number` | Pixel width (photos/video) |
| `isSketch` | `boolean` | `true` if this is a drawing. Reflected in `featureFlags` bit `0x0040`. |
| `favorite` | `boolean` | Whether the user has favorited this attachment |
| `creationDevice` | `string` | Human-readable device name that captured the media |
| `creationDeviceIdentifier` | `string` | Device UUID |
| `thumbnail` | `object` | Low-resolution preview metadata |
| `thumbnailContentType` | `string` | MIME type of the thumbnail |
| `duration` | `number` | Duration in seconds for audio/video. Absent for photos/PDFs. |
| `audioChannels` | `string` | e.g. `"stereo"`. Audio only. |
| `format` | `string` | Audio/video codec or container format |
| `recordingDevice` | `string` | Microphone or capture device description. Audio only. |
| `sampleRate` | `string` | Audio sample rate (e.g. `"44100"`). |
| `timeZoneName` | `string` | IANA time zone where media was captured |
| `location` | `object` | GPS location where media was captured. Same shape as entry-level `location`. |
| `pdfName` | `string` | Original filename for PDFs |
| `title` | `string` | Optional user-provided caption or title |

### `clientMeta`

Information about the device and app that created or last saved this entry.

```json
{
  "deviceId": "device-uuid",
  "deviceName": "Murphy's iPhone",
  "creationDevice": "iPhone 15 Pro",
  "creationDeviceModel": "iPhone16,2",
  "creationDeviceType": "iPhone",
  "creationOSName": "iOS",
  "creationOSVersion": "17.2",
  "browserName": null,
  "browserVersion": null,
  "creationAIHost": "api.anthropic.com",
  "creationAIModel": "claude-opus-4-6",
  "source": null
}
```

| Field | Type | Notes |
|---|---|---|
| `deviceId` | `string` | UUID of the device that last saved this revision |
| `deviceName` | `string` | User-assigned device name (e.g. "Murphy's iPhone") |
| `creationDevice` | `string` | Marketing name of the creation device |
| `creationDeviceModel` | `string` | Device model identifier (e.g. `"iPhone16,2"`) |
| `creationDeviceType` | `string` | Device category (e.g. `"iPhone"`, `"iPad"`, `"Mac"`, `"Web"`) |
| `creationOSName` | `string` | OS name (e.g. `"iOS"`, `"macOS"`, `"Android"`) |
| `creationOSVersion` | `string` | OS version string |
| `browserName` | `string` | Browser name for web-created entries. `null` on native apps. |
| `browserVersion` | `string` | Browser version for web-created entries. `null` on native apps. |
| `creationAIHost` | `string \| null` | AI API host when entry was AI-assisted. Present when `featureFlags` bit `0x8000` is set. |
| `creationAIModel` | `string \| null` | AI model identifier when entry was AI-assisted. Present when `featureFlags` bit `0x8000` is set. |
| `source` | `string \| null` | Optional source identifier for generated or migrated entries. |

---

## Feature flags

`featureFlags` is a lowercase hexadecimal string (no `0x` prefix) where each bit indicates a feature used in the entry. Clients use this to detect content that may require capabilities they don't support.

**Format rules:**
- Always present; use `"0"` if no features are set
- Lowercase hex only, no `0x` prefix
- Multiple features are combined with bitwise OR
- Example: entry with RTJ content and a video → `0x0010 | 0x0020 = 0x0030` → `"30"`

### Bit table

| Bit | Hex | Name | Description |
|---|---|---|---|
| 0 | `0x0001` | Multiple Attachments | Entry contains more than one attachment |
| 1 | `0x0002` | GIF or PNG Photos | Entry contains non-JPEG photos (GIF or PNG) |
| 2 | `0x0004` | Location User Label | **Deprecated.** Clients must ignore. |
| 3 | `0x0008` | Audio | Entry contains an audio attachment |
| 4 | `0x0010` | Rich Text JSON | Entry content is stored as RTJ, not Markdown. `richTextJSON` is the canonical content field. |
| 5 | `0x0020` | Video | Entry contains a video attachment |
| 6 | `0x0040` | Drawing | Entry contains a photo flagged as a drawing (`moment.isSketch = true`) |
| 7 | `0x0080` | PDF | Entry contains a PDF attachment |
| 8 | `0x0100` | Highlighting | Entry text contains highlighted spans (`highlightedColor` attribute in RTJ) |
| 9 | `0x0200` | Pinned | Entry is pinned (`isPinned = true`) |
| 10 | `0x0400` | Test Flag | Forces "unknown content" warning regardless of other flags. Used for testing. |
| 11 | `0x0800` | Entry Type | `entryType` field is non-empty (e.g. `"welcome-entry"`) |
| 12 | `0x1000` | Extended Embed Types (2023) | RTJ contains any of: `contact`, `location`, `podcast`, `song`, `motionActivity` (step count), `workout` embedded objects |
| 13 | `0x2000` | Extended Embed Types V2 (2024, iOS 18) | RTJ contains any of: `genericMedia`, `stateOfMind`, or `motionActivity` with `movementType` field |
| 14 | `0x4000` | Underlined | Entry text contains underlined spans |
| 15 | `0x8000` | AI Extended Metadata | `clientMeta.creationAIHost` and `clientMeta.creationAIModel` are populated |
| 16 | `0x10000` | Route Data | `location.route` is present with extended GPS route data |

### Client behavior requirements

- When a client receives an entry containing any bit it does not implement, it **must** warn the user that editing may cause content loss.
- When the user acknowledges the warning, the client **may** allow editing to proceed.
- Bit `0x0400` (Test Flag) always triggers the unknown-content warning regardless of client capability.
- When writing an entry, the client **must** set `featureFlags` to the bitwise OR of all features present in the content and metadata at save time.

### Example

`featureFlags = "5b3"` decodes as:

```
0x5b3 = 0x0001 | 0x0002 | 0x0010 | 0x0020 | 0x0080 | 0x0100 | 0x0400
```

→ Multiple Attachments, GIF/PNG, Rich Text JSON, Video, PDF, Highlighting, Test Flag.

Use [`feature-flag-tool.html`](feature-flag-tool.html) to interactively build and decode `featureFlags` values.

---

## Complete example

Minimal valid entry with RTJ content, one photo, and a location:

```json
{
  "id": "A1B2C3D4E5F67890A1B2C3D4E5F67890",
  "date": 1700000000000,
  "userEditDate": 1700000060000,
  "timeZone": "America/Los_Angeles",
  "isAllDay": false,
  "isPinned": false,
  "starred": false,
  "featureFlags": "11",
  "richTextJSON": {
    "meta": { "version": 1, "created": { "platform": "com.bloombuilt.dayone-ios", "version": 305 } },
    "nodes": [
      { "text": "Had a great hike today.", "attributes": { "line": {} } }
    ]
  },
  "tags": ["hiking"],
  "activity": "Walking",
  "location": {
    "latitude": 37.8651,
    "longitude": -119.5383,
    "altitude": 1200.0,
    "heading": 90.0,
    "speed": 1.2,
    "placeName": "Yosemite Valley",
    "localityName": "Yosemite Village",
    "administrativeArea": "California",
    "country": "United States",
    "streetAddress": "",
    "timeZoneName": "America/Los_Angeles",
    "region": {
      "identifier": "yosemite-region",
      "latitude": 37.8651,
      "longitude": -119.5383,
      "radius": 500.0
    }
  },
  "weather": {
    "description": "Sunny",
    "tempCelsius": 22.0
  },
  "moments": [
    {
      "id": "MOMENT-UUID-1234",
      "type": "photo",
      "contentType": "image/jpeg",
      "md5": "d41d8cd98f00b204e9800998ecf8427e",
      "date": 1700000000000,
      "createdAt": 1700000000000,
      "height": 3024,
      "width": 4032,
      "isSketch": false,
      "favorite": false,
      "creationDevice": "iPhone 15 Pro",
      "creationDeviceIdentifier": "DEVICE-UUID",
      "thumbnail": {
        "md5": "abc123",
        "contentType": "image/jpeg",
        "fileSize": 8192,
        "height": 256,
        "width": 341
      },
      "thumbnailContentType": "image/jpeg"
    }
  ],
  "clientMeta": {
    "deviceId": "DEVICE-UUID",
    "deviceName": "Murphy's iPhone",
    "creationDevice": "iPhone 15 Pro",
    "creationDeviceModel": "iPhone16,2",
    "creationDeviceType": "iPhone",
    "creationOSName": "iOS",
    "creationOSVersion": "17.2"
  },
  "ownerUserId": "USER-UUID",
  "creatorUserId": "USER-UUID",
  "editorUserId": "USER-UUID",
  "templateID": null,
  "promptID": null,
  "unread_marker_id": null
}
```

(`featureFlags = "11"` = `0x0001` Multiple Attachments + `0x0010` Rich Text JSON)
