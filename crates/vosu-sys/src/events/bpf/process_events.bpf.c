// SPDX-License-Identifier: MIT OR Apache-2.0
#include <linux/bpf.h>
#include <linux/errno.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>

#define TASK_COMM_LEN 16
#define MAX_CANDIDATES 128
#define COMM_HINT_MAX_LEN 15
#define MAX_COMM_HINTS 256
#define EVENT_INSPECT 1
#define EVENT_EXIT 2

typedef struct {
    int counter;
} atomic_t;

struct signal_struct {
    atomic_t live;
} __attribute__((preserve_access_index));

struct task_struct {
    int pid;
    int tgid;
    struct task_struct *group_leader;
    char comm[TASK_COMM_LEN];
    struct signal_struct *signal;
} __attribute__((preserve_access_index));

struct process_event {
    __u32 tgid;
    __u8 kind;
    __u8 reserved[3];
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_CANDIDATES);
    __type(key, __u32);
    __type(value, __u8);
} candidates SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_CANDIDATES);
    __type(key, __u32);
    __type(value, __u8);
} tracked SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, MAX_CANDIDATES);
    __type(key, __u32);
    __type(value, __u8);
} tombstones SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 64 * 1024);
} events SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} dropped SEC(".maps");

/*
 * comm_hints (D18, plugin-abi.md Sec 6.1) is filled from userspace
 * (vosu-sys::events::ebpf::EbpfCommFilter) with up to MAX_COMM_HINTS
 * substrings of at most COMM_HINT_MAX_LEN bytes each. Key layout mirrors
 * Rust's comm_hint_key: byte 0 is the hint length, bytes 1..=15 are the
 * hint content, right-padded with zero past that length.
 */
struct comm_hint_key {
    __u8 len;
    char bytes[COMM_HINT_MAX_LEN];
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_COMM_HINTS);
    __type(key, struct comm_hint_key);
    __type(value, __u8);
} comm_hints SEC(".maps");

/*
 * comm_hint_len_slots holds one entry per possible hint length (index
 * `len - 1`, length 1..=COMM_HINT_MAX_LEN): value `len` means comm_hints
 * currently has at least one hint of that length, value 0 means skip.
 * Userspace (EbpfCommFilter::set_comm_hints) widens this to the union of
 * the current and target length sets before diffing comm_hints, then
 * narrows it to the target set once the diff lands, so it is always a
 * superset of the lengths actually present. candidate_comm reads one slot
 * per outer-loop iteration and skips the inner window scan entirely for
 * any length with no registered hint, instead of always probing all 15
 * lengths against comm_hints.
 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, COMM_HINT_MAX_LEN);
    __type(key, __u32);
    __type(value, __u8);
} comm_hint_len_slots SEC(".maps");

static __always_inline void record_drop(void)
{
    __u32 key = 0;
    __u64 *count = bpf_map_lookup_elem(&dropped, &key);

    if (count)
        __sync_fetch_and_add(count, 1);
}

/*
 * In-kernel comm_hints matching.
 *
 * The BPF verifier cannot do data-dependent substring search against an
 * arbitrary-length value, so this does not scan the (up to MAX_COMM_HINTS)
 * stored hints. Instead, for each registered hint length it walks every
 * starting byte of `comm` (TASK_COMM_LEN bytes) and probes the comm_hints
 * hash map with the exact {len, bytes} key for that window.
 *
 * Earlier revisions built that window loop with #pragma unroll, giving the
 * compiler a separate copy of the `bpf_map_lookup_elem` call site -- with
 * its own runtime-gated branches (NUL-run cap, registered-length check) --
 * for every one of the up to 15*15 (start, len) combinations. Each gated
 * call site is a fork point the verifier must walk both sides of, and those
 * forks compound multiplicatively across copies; on a real kernel that hit
 * the 1,000,000-processed-instruction verifier budget (total_states 81,783
 * vs. 2,754 at the hardcoded-filter baseline) despite a modest *static*
 * instruction count. A later revision moved the window scan into
 * bpf_loop() (BPF_FUNC_loop, Linux 5.17+) to get a single call site back,
 * but that raised this file's floor above the ring buffer's 5.8 for no
 * matching functional need.
 *
 * This version instead keeps both loops as plain, non-unrolled, bounded
 * `for` loops: the outer loop (COMM_HINT_MAX_LEN slots) and the inner loop
 * (TASK_COMM_LEN start positions) each have a fixed, compile-time trip
 * count and no #pragma unroll, so the verifier uses the "bounded loops"
 * feature (Linux 5.3+): it simulates each loop iteration by iteration,
 * pruning a branch once its state is provably equivalent to one already
 * explored, instead of unrolling the loop body at compile time or needing
 * a bpf_loop() callback. Because the translation unit still only contains
 * one occurrence of the `bpf_map_lookup_elem` call against comm_hints --
 * state pruning collapses the repeated per-iteration visits back down,
 * it does not multiply the call site the way unrolling would -- that is
 * the same property the bpf_loop() callback had, without requiring 5.17.
 *
 * Two bounded, data-dependent cuts keep the common non-matching case cheap
 * without adding a verifier-unbounded loop:
 *   - comm is always NUL-padded (the raw kernel `comm` field, and every
 *     probe-read helper used to fill it, zero-fill past the terminator),
 *     and no registered hint may contain a NUL (CommHints::new rejects
 *     that), so a window reaching comm's first NUL byte can never match.
 *     comm_len (the run of non-NUL bytes from byte 0) bounds every
 *     length's start positions, so the inner loop breaks before walking
 *     past the terminator.
 *   - comm_hint_len_slots (set from userspace) limits the outer loop to
 *     lengths that actually have a registered hint, instead of probing
 *     comm_hints for all 15 lengths unconditionally.
 * With the default two hints (lengths 4 and 12) a non-matching short comm
 * like "bash" (comm_len 4) now costs 15 cheap comm_hint_len_slots lookups
 * (a COMM_HINT_MAX_LEN-entry BPF_MAP_TYPE_ARRAY, bounded and cheap) plus one
 * comm_hints lookup for length 4's one valid start position and zero for
 * length 12 (12 > comm_len) -- one comm_hints lookup total, not the
 * previous worst case of up to 16 gated lookups or the original hardcoded
 * filter's fixed 135.
 *
 * Minimum kernel: 5.8, set by the ring buffer map (`events`) elsewhere in
 * this file -- comm_hint matching above needs nothing newer than the
 * bounded-loops verifier feature (5.3).
 */
static __always_inline int candidate_comm(const char comm[TASK_COMM_LEN])
{
    __u8 comm_len = TASK_COMM_LEN;
#pragma unroll
    for (int i = 0; i < TASK_COMM_LEN; i++) {
        if (comm[i] == 0) {
            comm_len = (__u8)i;
            break;
        }
    }
    if (comm_len == 0)
        return 0;

    char local_comm[TASK_COMM_LEN];
    __builtin_memcpy(local_comm, comm, TASK_COMM_LEN);

    struct comm_hint_key key = {};

#pragma clang loop unroll(disable)
    for (__u8 len = 1; len <= COMM_HINT_MAX_LEN; len++) {
        /* `len` is the loop's own induction variable, not a value trusted
         * from the map read, so the verifier tracks its range (1..=15)
         * precisely and cheaply the same way it would for any ordinary
         * bounded counting loop. A length taken straight from the map's
         * runtime byte would carry that value's full 0..255 range into
         * every later comparison and loop bound, which the verifier
         * cannot resolve against `comm_len`'s precisely tracked range
         * without the search blowing past its complexity budget
         * ("infinite loop detected" despite both loops being statically
         * bounded). `slot` is only ever used as a map key, so its
         * precision does not matter. */
        __u32 slot = (__u32)(len - 1);
        __u8 *slot_ptr = bpf_map_lookup_elem(&comm_hint_len_slots, &slot);

        if (!slot_ptr || *slot_ptr == 0)
            continue;
        if (len > comm_len)
            continue;

        key.len = len;

#pragma clang loop unroll(disable)
        for (__u32 start = 0; start < TASK_COMM_LEN; start++) {
            if (start + len > comm_len)
                break;

#pragma clang loop unroll(full)
            for (int i = 0; i < COMM_HINT_MAX_LEN; i++) {
                if (i < len && start + i < TASK_COMM_LEN)
                    key.bytes[i] = local_comm[start + i];
            }

            if (bpf_map_lookup_elem(&comm_hints, &key))
                return 1;
        }
    }

    return 0;
}

static __always_inline int emit_event(__u32 tgid, __u8 kind)
{
    struct process_event *event;

    event = bpf_ringbuf_reserve(&events, sizeof(*event), 0);
    if (!event) {
        record_drop();
        return 0;
    }

    event->tgid = tgid;
    event->kind = kind;
    __builtin_memset(event->reserved, 0, sizeof(event->reserved));
    bpf_ringbuf_submit(event, 0);
    return 0;
}

static __always_inline int clear_candidate(__u32 tgid)
{
    long result;

    result = bpf_map_delete_elem(&candidates, &tgid);
    if (result && result != -ENOENT) {
        record_drop();
        return -1;
    }
    return 0;
}

static __always_inline int clear_generation(__u32 tgid)
{
    long result;

    if (clear_candidate(tgid))
        return -1;
    result = bpf_map_delete_elem(&tracked, &tgid);
    if (result && result != -ENOENT) {
        record_drop();
        return -1;
    }
    result = bpf_map_delete_elem(&tombstones, &tgid);
    if (result && result != -ENOENT) {
        record_drop();
        return -1;
    }
    return 0;
}

static __always_inline int emit_candidate_inspection(__u32 tgid)
{
    __u8 tracked = 1;

    if (bpf_map_lookup_elem(&candidates, &tgid))
        return 0;

    if (bpf_map_update_elem(&candidates, &tgid, &tracked, BPF_NOEXIST)) {
        if (bpf_map_lookup_elem(&candidates, &tgid))
            return 0;

        record_drop();
        return 0;
    }

    return emit_event(tgid, EVENT_INSPECT);
}

static __always_inline int inspect_candidate(__u32 tgid,
                                             const char comm[TASK_COMM_LEN])
{
    if (!candidate_comm(comm))
        return 0;
    return emit_candidate_inspection(tgid);
}

SEC("raw_tracepoint/sched_process_exec")
int process_exec(struct bpf_raw_tracepoint_args *ctx)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    char comm[TASK_COMM_LEN] = {};

    if (bpf_get_current_comm(comm, sizeof(comm))) {
        record_drop();
        return 0;
    }

    /*
     * Fork cleared reused-TGID state. An exec stays in the same live
     * generation, so preserve tracked membership and the impossible-for-live
     * tombstone while forcing one post-exec candidate inspection.
     */
    if (clear_candidate(pid_tgid >> 32))
        return 0;

    return inspect_candidate(pid_tgid >> 32, comm);
}

SEC("raw_tracepoint/sched_process_fork")
int process_fork(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *child = (struct task_struct *)ctx->args[1];
    __u32 pid;
    __u32 tgid;

    if (bpf_core_read(&pid, sizeof(pid), &child->pid) ||
        bpf_core_read(&tgid, sizeof(tgid), &child->tgid)) {
        record_drop();
        return 0;
    }
    if (pid == tgid)
        clear_generation(tgid);
    return 0;
}

static __always_inline int emit_exit(struct task_struct *task)
{
    __u32 tgid;
    __u8 *candidate;
    __u8 tombstone = 1;
    long result;
    char comm[TASK_COMM_LEN] = {};

    if (bpf_core_read(&tgid, sizeof(tgid), &task->tgid)) {
        record_drop();
        return 0;
    }
    candidate = bpf_map_lookup_elem(&candidates, &tgid);
    if (!candidate) {
        if (!bpf_map_lookup_elem(&tracked, &tgid)) {
            if (BPF_CORE_READ_STR_INTO(&comm, task, group_leader, comm) < 0) {
                record_drop();
                return 0;
            }
            if (!candidate_comm(comm))
                return 0;
        }
    }

    result = bpf_map_update_elem(&tombstones, &tgid, &tombstone, BPF_NOEXIST);
    if (result) {
        if (bpf_map_lookup_elem(&tombstones, &tgid))
            return 0;
        record_drop();
        return 0;
    }

    result = bpf_map_delete_elem(&candidates, &tgid);
    if (result && result != -ENOENT)
        record_drop();
    result = bpf_map_delete_elem(&tracked, &tgid);
    if (result && result != -ENOENT)
        record_drop();
    return emit_event(tgid, EVENT_EXIT);
}

SEC("raw_tracepoint/sched_process_exit")
int process_exit(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *task = (struct task_struct *)ctx->args[0];
    struct signal_struct *signal;
    int live;

    /*
     * Linux before 6.16 does not expose group_dead as the second raw
     * tracepoint argument. do_exit() has already decremented signal->live,
     * so zero identifies group-dead state on those kernels.
     */
    if (bpf_core_read(&signal, sizeof(signal), &task->signal) ||
        !signal ||
        bpf_core_read(&live, sizeof(live), &signal->live.counter)) {
        record_drop();
        return 0;
    }
    if (live != 0)
        return 0;
    return emit_exit(task);
}

SEC("raw_tracepoint/sched_process_exit")
int process_exit_group_dead(struct bpf_raw_tracepoint_args *ctx)
{
    if (!ctx->args[1])
        return 0;
    return emit_exit((struct task_struct *)ctx->args[0]);
}

SEC("raw_tracepoint/task_rename")
int task_rename(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *task = (struct task_struct *)ctx->args[0];
    const char *newcomm = (const char *)ctx->args[1];
    char comm[TASK_COMM_LEN] = {};
    __u32 pid;
    __u32 tgid;
    if (bpf_probe_read_kernel_str(comm, sizeof(comm), newcomm) < 0) {
        record_drop();
        return 0;
    }

    if (bpf_core_read(&pid, sizeof(pid), &task->pid) ||
        bpf_core_read(&tgid, sizeof(tgid), &task->tgid)) {
        record_drop();
        return 0;
    }
    if (!candidate_comm(comm)) {
        long result;

        if (pid != tgid)
            return 0;
        result = bpf_map_delete_elem(&candidates, &tgid);
        if (result && result != -ENOENT)
            record_drop();
        return 0;
    }
    return emit_candidate_inspection(tgid);
}

/*
 * The BPF loader declaration below is metadata consumed by the kernel verifier.
 * It is not a source license identifier and does not alter the project license.
 */
char LICENSE[] SEC("license") = "GPL and additional rights";
