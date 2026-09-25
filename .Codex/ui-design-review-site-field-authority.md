# UI Design Review

The generated forms imply users can edit values the server will discard. Apply Norman's affordance principle: omit computed inputs, hide unreadable fields, and render readable but unwritable values as inert text. Share gating across validation and payload construction so hidden required controls cannot block saving. Keep existing form styling and labels; provide duration and base64 format hints. Browser regression coverage should assert visible controls and actual submitted payloads for both permitted and denied roles.
