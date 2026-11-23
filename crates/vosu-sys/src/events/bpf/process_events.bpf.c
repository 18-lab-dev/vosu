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
 * stored hints. Instead it enumerates every (start, length) window of the
 * fixed-size kernel `comm` field (TASK_COMM_LEN bytes) and probes the
 * comm_hints hash map with the exact {len, bytes} key for that window.
 *
 * The outer "start" loop is #pragma unroll, so "start" is a compile-time
 * constant in every unrolled copy of the body; "window" (bytes remaining
 * from "start", capped at COMM_HINT_MAX_LEN) is therefore also compile-time
 * per copy, which makes the inner byte-copy and length loops' bounds and
 * indices compile-time constants too. There is no data-dependent loop bound
 * anywhere in this function: the generated code is straight-line regardless
 * of how many hints comm_hints holds, bounding the cost to a fixed
 * triangular-sum count of lookups (15+14+...+1 = 120 for TASK_COMM_LEN=16,
 * COMM_HINT_MAX_LEN=15) per exec/exit/rename event.
 */
static __always_inline int candidate_comm(const char comm[TASK_COMM_LEN])
{
#pragma unroll
    for (int start = 0; start < TASK_COMM_LEN; start++) {
        struct comm_hint_key key = {};
        int window = TASK_COMM_LEN - start;

        if (window > COMM_HINT_MAX_LEN)
            window = COMM_HINT_MAX_LEN;

#pragma unroll
        for (int i = 0; i < COMM_HINT_MAX_LEN; i++) {
            if (i < window)
                key.bytes[i] = comm[start + i];
        }

#pragma unroll
        for (int len = COMM_HINT_MAX_LEN; len >= 1; len--) {
            if (len > window)
                continue;
            key.len = (__u8)len;
            if (bpf_map_lookup_elem(&comm_hints, &key))
                return 1;
            if (len > 1)
                key.bytes[len - 1] = 0;
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
