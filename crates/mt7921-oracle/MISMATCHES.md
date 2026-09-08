# Confirmed C/Rust semantic differences

## Disabled station BSS clears the active byte
+
+For `enable=false`, `encode_client_bss_command` writes zero to the BSS BASIC
+`active` byte. Pinned `mt76_connac_mcu_uni_add_bss` initializes `active=true`
+for station interfaces even when disabling and uses `conn_state=1` to request
+deactivation. Enabled station BSS requests match directly; the differential
+test normalizes only the disabled request's active byte.
+
+## Retained-GTK IGTK update loses the IGTK key ID
+
+For a two-cipher KEY_V2 update, `encode_key_v2_command` writes key ID zero into
+the second (IGTK) cipher regardless of its valid caller-supplied ID. Pinned
+`mt76_connac_mcu_sta_key_tlv` writes `key->keyidx` there. The public
+`encode_igtk_command` accepts IGTK IDs 4 and 5, so both valid calls differ at
+that byte. The retained GTK and all other KEY_V2 bytes match directly.
