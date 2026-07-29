use super::{MemoryError, ReadRequest};

const MERGED_READ_LIMIT: usize = 4096;

pub(super) fn read_exact_impl<F>(
    address: usize,
    dst: &mut [u8],
    mut read: F,
) -> Result<(), MemoryError>
where
    F: FnMut(usize, &mut [u8]) -> (bool, usize),
{
    let expected = dst.len();
    let (ok, count) = read(address, dst);
    if ok && count == expected {
        return Ok(());
    }
    dst.fill(0);
    Err(read_error(address, expected, count))
}

pub(super) fn read_many_impl<F>(
    reads: &mut [ReadRequest<'_>],
    mut read: F,
) -> Result<(), MemoryError>
where
    F: FnMut(usize, &mut [u8]) -> (bool, usize),
{
    if let [request] = reads {
        let result = read_single(request, &mut read);
        if result.is_err() {
            request.dst.fill(0);
        }
        result
    } else {
        read_many_batched(reads, &mut read)
    }
}

fn read_single<F>(request: &mut ReadRequest<'_>, read: &mut F) -> Result<(), MemoryError>
where
    F: FnMut(usize, &mut [u8]) -> (bool, usize),
{
    if request.dst.is_empty() {
        return Ok(());
    }

    let expected = request.dst.len();
    let (ok, count) = read(request.address, request.dst);
    let initialized = count.min(expected);
    if initialized < expected {
        let suffix_address =
            request
                .address
                .checked_add(initialized)
                .ok_or(MemoryError::AddressOverflow {
                    base: request.address,
                    offset: initialized,
                })?;
        let (suffix_ok, suffix_count) = read(suffix_address, &mut request.dst[initialized..]);
        if !suffix_ok || suffix_count != expected - initialized {
            return Err(read_error(
                suffix_address,
                expected - initialized,
                suffix_count,
            ));
        }
    } else if !ok {
        return Err(read_error(request.address, expected, count));
    }
    Ok(())
}

#[inline(never)]
fn read_many_batched<F>(reads: &mut [ReadRequest<'_>], read: &mut F) -> Result<(), MemoryError>
where
    F: FnMut(usize, &mut [u8]) -> (bool, usize),
{
    let mut scratch = [0u8; MERGED_READ_LIMIT];
    let result = (|| {
        let mut start = 0;
        while start < reads.len() {
            if reads[start].dst.is_empty() {
                start += 1;
                continue;
            }

            let group_address = reads[start].address;
            let page = group_address & !0xfff;
            let page_end = page | 0xfff;
            let merge_end = page_end.min(group_address.saturating_add(scratch.len() - 1));
            let mut span_end = group_address.checked_add(reads[start].dst.len()).ok_or(
                MemoryError::AddressOverflow {
                    base: group_address,
                    offset: reads[start].dst.len(),
                },
            )?;
            let mut end = start + 1;
            if span_end - 1 <= merge_end {
                while end < reads.len() && !reads[end].dst.is_empty() {
                    let request_end = reads[end].address.checked_add(reads[end].dst.len()).ok_or(
                        MemoryError::AddressOverflow {
                            base: reads[end].address,
                            offset: reads[end].dst.len(),
                        },
                    )?;
                    if reads[end].address < span_end || request_end - 1 > merge_end {
                        break;
                    }
                    span_end = request_end;
                    end += 1;
                }
            }

            if end == start + 1 {
                read_single(&mut reads[start], read)?;
                start = end;
                continue;
            }

            let span_len = span_end - group_address;
            let (ok, count) = read(group_address, &mut scratch[..span_len]);
            let initialized = count.min(span_len);
            if initialized == span_len && !ok {
                return Err(read_error(group_address, span_len, count));
            }
            for request in &mut reads[start..end] {
                let offset = request.address - group_address;
                let prefix = initialized.saturating_sub(offset).min(request.dst.len());
                if prefix != 0 {
                    request.dst[..prefix].copy_from_slice(&scratch[offset..offset + prefix]);
                }
                if prefix < request.dst.len() {
                    let suffix_address = request.address.checked_add(prefix).ok_or(
                        MemoryError::AddressOverflow {
                            base: request.address,
                            offset: prefix,
                        },
                    )?;
                    let (suffix_ok, suffix_count) =
                        read(suffix_address, &mut request.dst[prefix..]);
                    if !suffix_ok || suffix_count != request.dst.len() - prefix {
                        return Err(read_error(
                            suffix_address,
                            request.dst.len() - prefix,
                            suffix_count,
                        ));
                    }
                }
            }
            start = end;
        }
        Ok(())
    })();
    if result.is_err() {
        for request in reads {
            request.dst.fill(0);
        }
    }
    result
}

#[cold]
#[inline(never)]
fn read_error(address: usize, len: usize, count: usize) -> MemoryError {
    MemoryError::Read {
        address,
        len,
        message: format!("ReadProcessMemory returned {count} bytes"),
    }
}
