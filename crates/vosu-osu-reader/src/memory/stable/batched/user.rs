use super::*;

impl StableMemory {
    pub(in super::super) fn user_batched(&self) -> MemoryRead<User> {
        let (mut profile_object, mut login_status) = (Word::new(), Word::new());
        self.process.read_indirect_many(
            &mut [
                crate::process::IndirectReadRequest::new(
                    self.patterns.user_profile_ptr,
                    0,
                    &mut profile_object.bytes,
                ),
                crate::process::IndirectReadRequest::new(
                    self.patterns.raw_login_status_ptr,
                    0,
                    &mut login_status.bytes,
                ),
            ],
            crate::process::ProcessBitness::X86,
        )?;
        let (b, login_status) = (profile_object.ptr(0), login_status.ptr(0));
        if b == 0 {
            return self.not_ready();
        }
        let mut fields = Span::<4, 0xac>::new();
        self.read_batch([fields.request(b)?])?;
        let [name] = self.strings([fields.ptr(0x30)])?;
        Ok(ReadStatus::Ready(User {
            name,
            accuracy: fields.f64(4),
            ranked_score: js_long(fields.i64(0xc)),
            id: fields.i32(0x70),
            level: fields.f32(0x74) as f64,
            play_count: fields.i32(0x7c),
            play_mode: fields.i32(0x80),
            rank: fields.i32(0x84),
            country_code: fields.i32(0x9c),
            performance_points: fields.i32(0x88) as f64,
            raw_bancho_status: fields.u8(0x8c) as i8 as i64,
            background_colour: fields.u32(0xac),
            raw_login_status: login_status as i64,
            matchmaking: None,
        }))
    }
}
