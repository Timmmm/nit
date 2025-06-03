use crate::leb128::{leb128_to_u32, u32_to_leb128};
use anyhow::{Result, anyhow, bail};

pub fn make_custom_section(name: &str, content: &[u8]) -> Vec<u8> {
    // A custom section is:
    //
    // * One-byte section ID (0 for custom sections).
    // * u32 size of the section contents (not including the section type or size u32).
    // * Contents:
    //   * Name of the section, as a UTF-8 string, prefixed with a u32 length encoded via LEB128.
    //   * Data

    let name_len = name.len() as u32;

    assert!(
        name_len as usize == name.len(),
        "Custom section name is too long"
    );

    let name_len_leb128 = u32_to_leb128(name_len);

    let content_len = name_len_leb128.len() + name.len() + content.len();
    let content_len_u32 = content_len as u32;

    assert!(
        content_len_u32 as usize == content_len,
        "Custom section content is too long"
    );

    let content_len_leb128 = u32_to_leb128(content_len_u32);

    let mut section = Vec::with_capacity(1 + content_len_leb128.len() + content_len);

    // Section ID (type).
    section.push(0);

    // Content size.
    section.extend_from_slice(&content_len_leb128);

    // Content.
    section.extend_from_slice(&name_len_leb128);
    section.extend_from_slice(name.as_bytes());
    section.extend_from_slice(content);

    section
}

/// Find all custom sections in a WASM file with the given name. Note that
/// for WASM components we do not recurse into modules so this will only
/// find custom sections at the top level of the component.
///
/// This returns the byte ranges of the entire section if you want to remove it,
/// and also a byte slice to the section contents if you want to read it.
pub fn find_custom_sections<'a>(
    bytes: &'a [u8],
    name: &str,
) -> Result<(Vec<std::ops::Range<usize>>, Vec<&'a [u8]>)> {
    if bytes.len() < 8 {
        bail!(
            "WASM file is too short to be valid: found {} bytes, need >=8",
            bytes.len()
        );
    }
    if &bytes[0..4] != b"\0asm" {
        bail!(
            "WASM file does not start with the magic number '\0asm': found {:?}",
            &bytes[0..4]
        );
    }

    // The file can either be a WASM module or a WASM component (which contains modules).
    // For WASM modules the version is 0x01 0x00 and the layer is 0x00 0x00 (though
    // the spec currently just says it's all one version field with the value 0x01 0x00 0x00 0x00).
    //
    // For components, the layer is 0x01 0x00 and the version is currently 0x0D 0x00 but
    // that will change so I'll ignore it.

    let version = &bytes[4..6];
    let layer = &bytes[6..8];

    if layer == &[0, 0] {
        if version != &[1, 0] {
            bail!(
                "WASM module does not have the expected version 1.0: found {:?}",
                version
            );
        }
    } else if layer == &[1, 0] {
        if version != &[13, 0] {
            bail!(
                "WASM component does not have the expected version 13.0: found {:?}",
                version
            );
        }
    } else {
        bail!(
            "WASM file does not have an expected layer ([0, 0] or [1, 0]): found {:?}",
            layer
        );
    }

    let mut section_ranges = Vec::new();
    let mut section_contents = Vec::new();

    // Start after the magic number and version.
    let mut section_start = 8;
    while section_start < bytes.len() {
        let mut offset = section_start;

        // Read section ID.
        let section_id = bytes[offset];
        offset += 1;

        // Read section size.
        let (section_size, incr) = leb128_to_u32(&bytes[offset..]).ok_or_else(|| {
            anyhow!(
                "Failed to read section size: offset {offset}, file size {}",
                bytes.len()
            )
        })?;
        let section_size = section_size as usize;
        offset += incr;

        // Check we have enough bytes for the section.
        if offset + section_size > bytes.len() {
            bail!(
                "Section size {section_size} exceeds remaining bytes in the file: offset {offset}, file size {}",
                bytes.len()
            );
        }

        let next_section_start = offset + section_size;

        // If it's a custom section, read its name and contents.
        if section_id == 0 {
            // Read the name length.
            let (name_len, incr) = leb128_to_u32(&bytes[offset..offset + section_size])
                .ok_or_else(|| {
                    anyhow!(
                        "Failed to read custom section name length: offset {offset}, file size {}",
                        bytes.len()
                    )
                })?;
            let name_len = name_len as usize;
            offset += incr;

            // Check we have enough bytes for the name.
            if incr + name_len > section_size {
                bail!("Custom section name length {name_len} exceeds section contents size");
            }

            // Read the name.
            let section_name_bytes = &bytes[offset..offset + name_len];
            let section_name = std::str::from_utf8(section_name_bytes).map_err(|_| {
                anyhow!(
                    "Custom section name is not valid UTF-8: {:?}",
                    section_name_bytes
                )
            })?;
            offset += name_len;

            // If the name matches, add the section contents to the result.
            if section_name == name {
                section_ranges.push(section_start..next_section_start);
                section_contents.push(&bytes[offset..next_section_start]);
            }
        }

        section_start = next_section_start;
    }

    Ok((section_ranges, section_contents))
}
