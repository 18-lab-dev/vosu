use super::*;

pub struct StableMemory {
    pub(super) process: ProcessHandle,
    pub(super) patterns: StablePatterns,
    pub(super) config_positions: Vec<usize>,
    pub(super) binding_positions: Vec<usize>,
    pub(super) config_identity: usize,
    pub(super) binding_identity: usize,
    pub(super) chat_address: usize,
    pub(super) mania_speed_address: usize,
    pub(super) gameplay_mode: i64,
    pub(super) ruleset_container: usize,
    pub(super) status_container: usize,
    pub(super) play_time_container: usize,
    pub show_mp_commands: bool,
    pub read_mania_scroll_speed: bool,
}

impl StableMemory {
    pub fn new(process: ProcessHandle, patterns: StablePatterns) -> Self {
        Self {
            process,
            patterns,
            config_positions: vec![],
            binding_positions: vec![],
            config_identity: 0,
            binding_identity: 0,
            chat_address: 0,
            mania_speed_address: 0,
            gameplay_mode: 0,
            ruleset_container: 0,
            status_container: 0,
            play_time_container: 0,
            show_mp_commands: false,
            read_mania_scroll_speed: true,
        }
    }
    pub fn resolve(process: ProcessHandle) -> MemoryRead<Self> {
        Ok(match StablePatterns::resolve(&process)? {
            ReadStatus::Ready(p) => ReadStatus::Ready(Self::new(process, p)),
            ReadStatus::NotReady => ReadStatus::NotReady,
            ReadStatus::Unchanged => ReadStatus::Unchanged,
            ReadStatus::Unsupported(s) => ReadStatus::Unsupported(s),
        })
    }
    pub fn invalidate_settings_cache(&mut self) {
        self.config_positions.clear();
        self.binding_positions.clear();
        self.config_identity = 0;
        self.binding_identity = 0;
        self.ruleset_container = 0;
        self.status_container = 0;
        self.play_time_container = 0;
    }
    pub(super) fn with_random_seed(
        &self,
        mut value: CalculateMods,
        ruleset: usize,
        mode: i64,
    ) -> Result<CalculateMods, MemoryError> {
        if mode != 3 {
            return Ok(value);
        }
        let Some(random) = value.array.iter_mut().find(|item| item.acronym == "RD") else {
            return Ok(value);
        };
        let mut settings = IndexMap::new();
        settings.insert(
            "seed".to_owned(),
            json!(self.process.read_i32(self.add(ruleset, 0x118)?)?),
        );
        random.settings = Some(settings);
        Ok(mods_from_array(value.array))
    }
    pub(super) fn add(&self, b: usize, o: usize) -> Result<usize, MemoryError> {
        checked_address_add(b, o)
    }
    pub(super) fn i32(&self, a: usize) -> Result<i64, MemoryError> {
        Ok(self.process.read_i32(a)? as i64)
    }
    pub(super) fn ptr(&self, a: usize) -> Result<usize, MemoryError> {
        Ok(self.process.read_u32(a)? as usize)
    }
    pub(super) fn ptr2(&self, a: usize) -> Result<usize, MemoryError> {
        self.ptr(self.ptr(a)?)
    }
    pub(super) fn string(&self, a: usize) -> Result<String, MemoryError> {
        match self.process.read_stable_string(a)? {
            ReadStatus::Ready(v) => Ok(v),
            _ => Ok(String::new()),
        }
    }
    pub(super) fn string_ptr(&self, a: usize) -> Result<String, MemoryError> {
        self.string(self.ptr(a)?)
    }
    pub(super) fn ruleset(&mut self) -> Result<usize, MemoryError> {
        let result = (|| {
            let anchor =
                self.patterns
                    .rulesets_addr
                    .checked_sub(0xb)
                    .ok_or(MemoryError::AddressOverflow {
                        base: self.patterns.rulesets_addr,
                        offset: 0xb,
                    })?;
            if self.ruleset_container == 0 {
                let container = self.ptr(anchor)?;
                let ruleset = self.ptr(self.add(container, 4)?)?;
                Ok((container, ruleset))
            } else {
                let mut container_bytes = [0; 4];
                let mut ruleset_bytes = [0; 4];
                let batch = self.process.read_many(&mut [
                    ReadRequest::new(anchor, &mut container_bytes),
                    ReadRequest::new(
                        self.add(self.ruleset_container, 4)?,
                        &mut ruleset_bytes,
                    ),
                ]);
                if batch.is_err() {
                    let container = self.ptr(anchor)?;
                    let ruleset = self.ptr(self.add(container, 4)?)?;
                    Ok((container, ruleset))
                } else {
                    let container = u32::from_le_bytes(container_bytes) as usize;
                    if container == self.ruleset_container {
                        Ok((container, u32::from_le_bytes(ruleset_bytes) as usize))
                    } else {
                        Ok((container, self.ptr(self.add(container, 4)?)?))
                    }
                }
            }
        })();
        match result {
            Ok((container, ruleset)) => {
                self.ruleset_container = container;
                Ok(ruleset)
            }
            Err(error) => {
                self.ruleset_container = 0;
                Err(error)
            }
        }
    }
    fn cached_i32_chain(
        &self,
        anchor: usize,
        cached_container: usize,
        signed: bool,
    ) -> Result<(usize, i64), MemoryError> {
        if cached_container == 0 {
            let container = self.ptr(anchor)?;
            let value = if signed {
                self.process.read_i32(container)? as i64
            } else {
                self.process.read_u32(container)? as i64
            };
            return Ok((container, value));
        }
        let mut container_bytes = [0; 4];
        let mut value_bytes = [0; 4];
        let batch = self.process.read_many(&mut [
            ReadRequest::new(anchor, &mut container_bytes),
            ReadRequest::new(cached_container, &mut value_bytes),
        ]);
        if batch.is_err() {
            let container = self.ptr(anchor)?;
            let value = if signed {
                self.process.read_i32(container)? as i64
            } else {
                self.process.read_u32(container)? as i64
            };
            return Ok((container, value));
        }
        let container = u32::from_le_bytes(container_bytes) as usize;
        if container != cached_container {
            let value = if signed {
                self.process.read_i32(container)? as i64
            } else {
                self.process.read_u32(container)? as i64
            };
            return Ok((container, value));
        }
        let value = if signed {
            i32::from_le_bytes(value_bytes) as i64
        } else {
            u32::from_le_bytes(value_bytes) as i64
        };
        Ok((container, value))
    }
    pub(super) fn status_value(&mut self) -> Result<i64, MemoryError> {
        match self.cached_i32_chain(
            self.patterns.status_ptr,
            self.status_container,
            false,
        ) {
            Ok((container, value)) => {
                self.status_container = container;
                Ok(value)
            }
            Err(error) => {
                self.status_container = 0;
                Err(error)
            }
        }
    }
    pub(super) fn play_time_value(&mut self) -> Result<i64, MemoryError> {
        let anchor = self.add(self.patterns.play_time_addr, 5)?;
        match self.cached_i32_chain(anchor, self.play_time_container, true) {
            Ok((container, value)) => {
                self.play_time_container = container;
                Ok(value)
            }
            Err(error) => {
                self.play_time_container = 0;
                Err(error)
            }
        }
    }
    pub(super) fn score_counts(&self, base: usize) -> Result<[i64; 8], MemoryError> {
        let mut tail = [0u8; 14];
        let mut maximum = [0u8; 2];
        self.process.read_many(&mut [
            ReadRequest::new(self.add(base, 0x88)?, &mut tail),
            ReadRequest::new(self.add(base, 0x68)?, &mut maximum),
        ])?;
        let mut values = [0i64; 8];
        for (index, bytes) in tail.chunks_exact(2).enumerate() {
            values[index] = i16::from_le_bytes([bytes[0], bytes[1]]) as i64;
        }
        values[7] = i16::from_le_bytes(maximum) as i64;
        Ok(values)
    }
    pub(super) fn not_ready<T>(&self) -> MemoryRead<T> {
        Ok(ReadStatus::NotReady)
    }
}
