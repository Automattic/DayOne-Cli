## RichTextJSON (RTJSON) Format Specification

- **Spec status**: Stable
- **Spec version**: 1
- **Last updated**: 2026-04-01

### Overview

Day One apps require a platform-neutral way to represent rich text with inline and block-level semantics, including media. RichTextJSON (RTJSON, abbreviated RTJ) is a simple JSON-based format designed for Day One's needs.

#### Goals

- Represent plain text with inline attributes (bold, italic, strike, inline code, highlighting, links)
- Represent line-level styling (headers, lists, indentation, quotes, code blocks)
- Support embedded media (photos, audio, video, PDFs) and external embeds (YouTube, Vimeo, Spotify)
- Support tables with inline rich text in cells
- Support Apple Journaling Suggestions content as structured embedded objects
- Be resilient to unknown attributes and future extensibility

#### Non-goals

- Full Markdown/HTML round-trip fidelity
- Arbitrary WYSIWYG layout or page formatting

---

### Top-level document structure

An RTJ document has the following structure:

```json
{
  "meta": {},
  "nodes": []
}
```

- `meta`: Metadata describing the document and creator
- `nodes`: Ordered sequence of content nodes (text or embedded). A single node is either text or embedded content, never both.

---

### Metadata

Example:

```json
{
  "meta": {
    "version": 1,
    "created": {
      "platform": "com.bloombuilt.dayone-ios",
      "version": 305
    }
  }
}
```

- `meta.version` (Integer): RTJ document structure version. Currently always `1`. If missing, assume `1`.
- `meta.created.platform` (String): Product identifier that created the document.
- `meta.created.version` (Integer): Creator product version used for migrations.

Notes:

- `version` reflects document structure. New text/line attributes may be introduced without changing `version` as long as older clients can parse structure and ignore unknown attributes.
- Other historical keys may exist and MUST be ignored by clients.

---

### Nodes

Content is a sequence of nodes. Each node is either a text node or an embedded content node. A node MUST NOT contain both `text` and `embeddedObjects`.

#### Text nodes

Minimal form:

```json
{ "text": "Hello, world!" }
```

Text nodes may include `attributes`.

```json
{
  "text": "Hello, world!",
  "attributes": {
    "bold": true,
    "strikethrough": true
  }
}
```

Consecutive nodes are concatenated for display. For example, the following corresponds to: `Hello, **world!**`.

```json
{
  "nodes": [
    { "text": "Hello, " },
    { "text": "world!", "attributes": { "bold": true } }
  ]
}
```

##### Text attributes

Apply to a span of text (the specific node). Supported keys:

- `bold` (Boolean)
- `italic` (Boolean)
- `strikethrough` (Boolean)
- `inlineCode` (Boolean): Render with fixed-width, code-style formatting
- `highlightedColor` (String): sRGB color as hexadecimal `RRGGBBAA`
- `linkURL` (String): URL string; render text as a link to this URL
- `autolink` (Boolean): Use the node's text as its link destination
- `cursorPlacement` (Boolean): Non-visual. If present, indicates editor cursor should be placed after this node on open. If multiple are present, use the first occurrence.
- `pageLink` (Boolean): Pre-release (Piano). Treat text as a link to a page whose name equals the node text
- `line` (Object): Line attributes (described below)

Notes:

- Unknown attributes MUST be ignored by clients.

##### Line attributes

Apply to an entire line of text, where a "line" is the contiguous sequence of text nodes up to a `\n` newline in the text content. Line attributes SHOULD be applied to all nodes within the line, but only the attribute on the first character of the line determines the line's style.

Example:

```json
{
  "text": "This is a header",
  "attributes": {
    "line": { "header": 1 }
  }
}
```

Supported line attributes:

- `header` (Integer): Levels `1`–`6` (1 is most prominent)
- `indentLevel` (Integer): Indentation level; may combine with list styles for nesting
- `listStyle` (String): One of `bulleted`, `numbered`, `checkbox`
- `checked` (Boolean): Only for `checkbox` items; ignored otherwise
- `listIndex` (Integer): Only for `numbered` items; if absent, number is previous numbered item + 1, or `1` if none
- `quote` (Boolean): Style as a block quote
- `codeBlock` (Boolean): Style as a code block
- `identifier` (String): Unique per line; used to identify items (e.g., checkbox IDs)

#### Embedded content nodes

Represents a single "paragraph" of embedded content (e.g., a photo, a photo collage, an audio recording, a YouTube video). Distinguished by `embeddedObjects` instead of `text`. An embedded node may include `attributes`, though shipping embedded objects do not currently use it.

Example:

```json
{
  "embeddedObjects": [
    { "type": "photo", "identifier": "ABCD123" },
    { "type": "photo", "identifier": "5555551234" }
  ]
}
```

Each embedded object has a `type` and type-specific required fields.

Supported embedded object types:

- `photo`

  ```json
  { "type": "photo", "identifier": "123" }
  ```

- `audio`

  ```json
  { "type": "audio", "identifier": "123" }
  ```

- `video`

  ```json
  { "type": "video", "identifier": "123" }
  ```

- `pdfAttachment`

  ```json
  { "type": "pdfAttachment", "identifier": "123" }
  ```

- `externalVideo` (YouTube/Vimeo)

  ```json
  {
    "type": "externalVideo",
    "url": "https://www.youtube.com/watch?v=ZNdzcFCKGYU"
  }
  ```

- `externalAudio` (Spotify)

  ```json
  {
    "type": "externalAudio",
    "url": "https://embed.spotify.com/?uri=spotify%3Atrack%3A<track_id_here>"
  }
  ```

- `renderableCodeBlock` (Markdown rendered to HTML in an embedded web view)

  ```json
  { "type": "renderableCodeBlock", "contents": "<b>Hello</b> *world*" }
  ```

- `horizontalLineRule`

  ```json
  { "type": "horizontalLineRule" }
  ```

- `table`

  ```json
  {
    "type": "table",
    "rows": [
      [
        { "content": [{ "text": "Name", "attributes": { "bold": true } }], "header": true },
        { "content": [{ "text": "Value", "attributes": { "bold": true } }], "header": true }
      ],
      [
        { "content": [{ "text": "foo" }] },
        { "content": [{ "text": "bar" }] }
      ]
    ],
    "markdown": "| **Name** | **Value** |\n| --- | --- |\n| foo | bar |"
  }
  ```

  Fields:
  - `rows` (Array, required): Ordered list of rows. Each row is an ordered list of cell objects.
  - `markdown` (String, optional): GitHub-flavored Markdown representation of the table. Written by clients during RTJson generation to allow native clients (iOS/Android) to display the table without parsing the structured `rows` data. Not parsed back during editing.

  Each cell object:
  - `content` (Array, required): Sequence of inline text segments. Each segment has:
    - `text` (String): The text content.
    - `attributes` (Object, optional): Inline formatting. Supported keys: `bold`, `italic`, `strikethrough`, `inlineCode`, `highlightedColor`, `linkURL`, `autolink`.
  - `header` (Boolean, optional): If `true`, the cell is a header cell (rendered as `<th>`). Header cells typically appear in the first row.

  Notes:
  - A `table` node MUST be the sole embedded object in its node.
  - Cell content is inline-only; block-level attributes (`line`) are not supported within cells.
  - Entries containing tables carry the feature flag `0x20000`.

- `dayOneEntry` (Currently unused). Note: Historical nodes may omit `journalID` created by old clients.

  ```json
  { "type": "dayOneEntry", "entryID": "12345", "journalID": "12345" }
  ```

- `preview` (Currently unused). Inserts a preview card for a web page.

  ```json
  { "type": "preview", "url": "https://dayoneapp.com" }
  ```

Embedded objects for Apple Journaling Suggestions:

- `contact`

  ```json
  {
    "type": "contact",
    "identifier": "some-uuid",
    "name": "Matt Mullenweg",
    "photoIdentifier": "12345",
    "source": "Apple Suggestions"
  }
  ```

- `location`

  ```json
  {
    "type": "location",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "city": "Draper",
    "placeName": "Living Planet Aquarium",
    "latitude": 40.334211,
    "longitude": 111.5225,
    "date": "2016-04-19T19:39:12Z",
    "source": "Apple Suggestions"
  }
  ```

- `motionActivity`

  ```json
  {
    "type": "motionActivity",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "startDate": "2016-04-19T19:39:12Z",
    "endDate": "2016-04-20T19:39:12Z",
    "steps": 525202,
    "movementType": "walking",
    "movementTypeName": "Walking",
    "source": "Apple Suggestions"
  }
  ```

- `podcast`

  ```json
  {
    "type": "podcast",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "show": "Random Podcast",
    "episode": "Episode 1",
    "artworkIdentifier": "1235B208EB214595A9ED976D894E2JLK",
    "date": "2016-04-19T19:39:12Z",
    "source": "Apple Suggestions"
  }
  ```

- `song`

  ```json
  {
    "type": "song",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "song": "Favorite Song",
    "artist": "Random Artist",
    "album": "Random Album",
    "artworkIdentifier": "1235B208EB214595A9ED976D894E2JLK",
    "date": "2016-04-19T19:39:12Z",
    "source": "Apple Suggestions"
  }
  ```

- `workout`

  ```json
  {
    "type": "workout",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "route": [
      [42.1411, 111.3415],
      [42.1324, 111.3563]
    ],
    "workoutMetrics": {
      "activeEnergyBurned": 242.0,
      "averageHeartRate": 180.0
    },
    "activityType": "bowling",
    "displayName": "Bowling",
    "startDate": "2016-04-19T19:39:12Z",
    "endDate": "2016-04-19T19:39:12Z",
    "distance": 14145.0,
    "source": "Apple Suggestions"
  }
  ```

- `genericMedia` (Available with iOS 18)

  ```json
  {
    "type": "genericMedia",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "title": "Song Title",
    "artist": "Random Artist",
    "album": "Random Album",
    "iconIdentifier": "1235B208EB214595A9ED976D894E2JLK",
    "date": "2016-04-19T19:39:12Z",
    "source": "Apple Suggestions"
  }
  ```

- `stateOfMind` (Available with iOS 18)

  ```json
  {
    "type": "stateOfMind",
    "identifier": "AF55B208EB214595A9ED976D894E2F4E",
    "kind": "momentaryEmotion",
    "kindDisplayName": "Momentary Emotion",
    "valence": 0.5,
    "valenceClassification": "veryPleasant",
    "valenceClassificationDisplayName": "Very Pleasant",
    "associations": ["friends", "family", "work"],
    "associationsDisplayNames": ["Friends", "Family", "Work"],
    "labels": ["calm", "confident", "excited"],
    "labelsDisplayNames": ["Calm", "Confident", "Excited"],
    "lightColor": "{\"stops\" : [{\"location\" : 0, \"color\" : \"#007AFFFF\"}, {\"color\" : \"#FF3B30FF\", \"location\" : 1}]}",
    "darkColor": "{\"stops\" : [{\"color\" : \"#FFCC00FF\", \"location\" : 0.5}, {\"location\" : 0.9, \"color\" : \"#34C759FF\"}]}",
    "iconIdentifier": "1235B208EB214595A9ED976D894E2JLK",
    "source": "Apple Suggestions"
  }
  ```

Grouping rules:

- The RTJ format allows arbitrary grouping of embedded objects within a node. Clients MAY impose additional constraints. Day One clients allow grouping photos and videos; other object types must be the sole embedded object in their node.

---

### Examples

Minimal text document:

```json
{
  "meta": { "version": 1 },
  "nodes": [
    { "text": "Hello, ", "attributes": { "italic": true } },
    { "text": "world!", "attributes": { "bold": true } },
    { "text": "\nA list:", "attributes": { "line": { "header": 3 } } },
    {
      "text": "Item one",
      "attributes": { "line": { "listStyle": "bulleted" } }
    },
    {
      "text": "Item two",
      "attributes": { "line": { "listStyle": "bulleted", "indentLevel": 1 } }
    },
    {
      "text": "Item three",
      "attributes": { "line": { "listStyle": "bulleted" } }
    },
    { "embeddedObjects": [{ "type": "horizontalLineRule" }] },
    {
      "embeddedObjects": [
        { "type": "photo", "identifier": "IMG_0001" },
        { "type": "photo", "identifier": "IMG_0002" }
      ]
    }
  ]
}
```

---

### Validation and interoperability

Clients SHOULD validate documents as follows:

- Top-level must include `nodes` array; `meta` object MAY be empty
- Each node MUST have exactly one of `text` (string) or `embeddedObjects` (non-empty array)
- `attributes`, when present, MUST be an object; unknown keys are allowed and MUST be ignored
- `highlightedColor` MUST be 8-hex-digit `RRGGBBAA`
- `linkURL` and `url` fields SHOULD be syntactically valid URLs
- Line attribute constraints:
  - `header` ∈ {1,2,3,4,5,6}
  - `listStyle` ∈ {"bulleted","numbered","checkbox"}
  - `checked` only meaningful when `listStyle` is `checkbox`
  - `listIndex` only meaningful when `listStyle` is `numbered`
- Embedded objects MUST include required fields per type (see type list above)

---

### Versioning and backward compatibility

- `meta.version` defines structural version of the document. Currently `1`.
- Introducing new attributes (text or line) that do not alter structure is allowed without changing `version`. Older clients MUST ignore unknown attributes and continue parsing.
- Structural changes (e.g., new node kinds, top-level keys) require incrementing `meta.version` and coordinated client support.

---

### Security and implementation notes

- `renderableCodeBlock.contents` is rendered to HTML in an embedded web view. Clients MUST sanitize and/or isolate rendering contexts to prevent script execution, cross-origin data leakage, or privilege escalation.
- External embeds (`externalVideo`, `externalAudio`, `preview`) SHOULD be fetched and rendered using safe, sandboxed mechanisms.
- Do not execute or dereference URLs during parse. Network fetches are a render-time concern.

---

### Appendix: JSON Schema (Draft 2020-12)

This schema is informative. Implementations MAY accept valid extensions that remain forward-compatible with the structural rules described above.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://dayoneapp.com/schemas/rtjson-1.schema.json",
  "title": "RichTextJSON v1",
  "type": "object",
  "additionalProperties": true,
  "required": ["nodes"],
  "properties": {
    "meta": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "version": { "type": "integer", "enum": [1] },
        "created": {
          "type": "object",
          "additionalProperties": true,
          "properties": {
            "platform": { "type": "string" },
            "version": { "type": "integer" }
          }
        }
      }
    },
    "nodes": {
      "type": "array",
      "items": { "$ref": "#/$defs/node" }
    }
  },
  "$defs": {
    "node": {
      "type": "object",
      "additionalProperties": true,
      "oneOf": [
        { "$ref": "#/$defs/textNode" },
        { "$ref": "#/$defs/embeddedNode" }
      ]
    },
    "textNode": {
      "type": "object",
      "required": ["text"],
      "properties": {
        "text": { "type": "string" },
        "attributes": { "$ref": "#/$defs/textAttributes" }
      },
      "additionalProperties": true
    },
    "textAttributes": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "bold": { "type": "boolean" },
        "italic": { "type": "boolean" },
        "strikethrough": { "type": "boolean" },
        "inlineCode": { "type": "boolean" },
        "highlightedColor": { "type": "string", "pattern": "^[0-9A-Fa-f]{8}$" },
        "linkURL": { "type": "string", "format": "uri" },
        "autolink": { "type": "boolean" },
        "cursorPlacement": { "type": "boolean" },
        "pageLink": { "type": "boolean" },
        "line": { "$ref": "#/$defs/lineAttributes" }
      }
    },
    "lineAttributes": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "header": { "type": "integer", "minimum": 1, "maximum": 6 },
        "indentLevel": { "type": "integer", "minimum": 0 },
        "listStyle": {
          "type": "string",
          "enum": ["bulleted", "numbered", "checkbox"]
        },
        "checked": { "type": "boolean" },
        "listIndex": { "type": "integer", "minimum": 1 },
        "quote": { "type": "boolean" },
        "codeBlock": { "type": "boolean" },
        "identifier": { "type": "string" }
      }
    },
    "embeddedNode": {
      "type": "object",
      "required": ["embeddedObjects"],
      "properties": {
        "embeddedObjects": {
          "type": "array",
          "minItems": 1,
          "items": { "$ref": "#/$defs/embeddedObject" }
        },
        "attributes": { "type": "object", "additionalProperties": true }
      },
      "additionalProperties": true
    },
    "tableCell": {
      "type": "object",
      "required": ["content"],
      "additionalProperties": true,
      "properties": {
        "content": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["text"],
            "additionalProperties": true,
            "properties": {
              "text": { "type": "string" },
              "attributes": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                  "bold": { "type": "boolean" },
                  "italic": { "type": "boolean" },
                  "strikethrough": { "type": "boolean" },
                  "inlineCode": { "type": "boolean" },
                  "highlightedColor": { "type": "string", "pattern": "^[0-9A-Fa-f]{8}$" },
                  "linkURL": { "type": "string", "format": "uri" },
                  "autolink": { "type": "boolean" }
                }
              }
            }
          }
        },
        "header": { "type": "boolean" }
      }
    },
    "embeddedObject": {
      "type": "object",
      "required": ["type"],
      "properties": {
        "type": { "type": "string" }
      },
      "allOf": [
        {
          "if": { "properties": { "type": { "const": "photo" } } },
          "then": { "required": ["identifier"] }
        },
        {
          "if": { "properties": { "type": { "const": "audio" } } },
          "then": { "required": ["identifier"] }
        },
        {
          "if": { "properties": { "type": { "const": "video" } } },
          "then": { "required": ["identifier"] }
        },
        {
          "if": { "properties": { "type": { "const": "pdfAttachment" } } },
          "then": { "required": ["identifier"] }
        },
        {
          "if": { "properties": { "type": { "const": "externalVideo" } } },
          "then": { "required": ["url"] }
        },
        {
          "if": { "properties": { "type": { "const": "externalAudio" } } },
          "then": { "required": ["url"] }
        },
        {
          "if": {
            "properties": { "type": { "const": "renderableCodeBlock" } }
          },
          "then": { "required": ["contents"] }
        },
        {
          "if": { "properties": { "type": { "const": "horizontalLineRule" } } },
          "then": { "required": [] }
        },
        {
          "if": { "properties": { "type": { "const": "table" } } },
          "then": { "required": ["rows"] }
        },
        {
          "if": { "properties": { "type": { "const": "dayOneEntry" } } },
          "then": {
            "required": ["entryID"],
            "properties": { "journalID": { "type": ["string", "null"] } }
          }
        },
        {
          "if": { "properties": { "type": { "const": "preview" } } },
          "then": { "required": ["url"] }
        },
        {
          "if": { "properties": { "type": { "const": "contact" } } },
          "then": { "required": ["identifier", "name"] }
        },
        {
          "if": { "properties": { "type": { "const": "location" } } },
          "then": { "required": ["identifier", "latitude", "longitude"] }
        },
        {
          "if": { "properties": { "type": { "const": "motionActivity" } } },
          "then": { "required": ["identifier", "startDate", "endDate"] }
        },
        {
          "if": { "properties": { "type": { "const": "podcast" } } },
          "then": { "required": ["identifier", "show"] }
        },
        {
          "if": { "properties": { "type": { "const": "song" } } },
          "then": { "required": ["identifier", "song", "artist"] }
        },
        {
          "if": { "properties": { "type": { "const": "workout" } } },
          "then": { "required": ["identifier", "startDate", "endDate"] }
        },
        {
          "if": { "properties": { "type": { "const": "genericMedia" } } },
          "then": { "required": ["identifier", "title"] }
        },
        {
          "if": { "properties": { "type": { "const": "stateOfMind" } } },
          "then": { "required": ["identifier", "kind", "valence"] }
        }
      ]
    }
  }
}
```

---

### Alternatives considered

Markdown provides many desired features and was previously used as a storage format. However, it does not support grouping photos into arbitrary collages or indenting an arbitrary paragraph independent of lists. RTJ addresses these gaps while retaining a simple, robust structure.
