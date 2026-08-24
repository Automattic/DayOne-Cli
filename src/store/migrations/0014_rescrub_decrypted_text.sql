-- Re-run the decrypted_text scrub for databases that already recorded migration 0012
-- before the revert window could write the legacy fallback again.

UPDATE entries
SET
  decrypted_text = NULL,
  data_json = json_remove(data_json, '$.decrypted_text', '$.payload.decrypted_text'),
  extra_fields_json = CASE
    WHEN extra_fields_json IS NULL THEN NULL
    WHEN json_type(extra_fields_json) = 'object'
         AND json_remove(extra_fields_json, '$.decrypted_text') = '{}'
      THEN NULL
    WHEN json_type(extra_fields_json) = 'object'
      THEN json_remove(extra_fields_json, '$.decrypted_text')
    ELSE extra_fields_json
  END
WHERE decrypted_text IS NOT NULL
   OR json_type(data_json, '$.decrypted_text') IS NOT NULL
   OR json_type(data_json, '$.payload.decrypted_text') IS NOT NULL
   OR (
     extra_fields_json IS NOT NULL
     AND json_type(extra_fields_json, '$.decrypted_text') IS NOT NULL
   );
