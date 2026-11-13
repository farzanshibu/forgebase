import 'dart:async';

import 'errors.dart';
import 'http.dart';
import 'realtime.dart';

const _j = '/api/v1/jobs';
String _enc(String s) => Uri.encodeComponent(s);

/// States of a job.
enum JobStatus {
  /// Waiting to run.
  queued,

  /// Running.
  running,

  /// Finished successfully.
  succeeded,

  /// Out of attempts.
  dead,

  /// Cancelled.
  cancelled;

  /// Parses the wire value (`queued` for unknown ones).
  static JobStatus parse(Object? s) =>
      JobStatus.values.firstWhere((v) => v.name == s, orElse: () => queued);

  /// True once a job can no longer change by itself (`dead` can still be
  /// retried by hand).
  bool get isFinal => this == succeeded || this == dead || this == cancelled;
}

/// States of a workflow.
enum WorkflowStatus {
  /// Steps are running.
  running,

  /// Every step succeeded.
  succeeded,

  /// A step failed.
  failed,

  /// Compensation jobs are running.
  compensating,

  /// Compensation finished.
  compensated,

  /// Cancelled.
  cancelled;

  /// Parses the wire value (`running` for unknown ones).
  static WorkflowStatus parse(Object? s) => WorkflowStatus.values
      .firstWhere((v) => v.name == s, orElse: () => running);

  /// True once a workflow has settled.
  bool get isFinal => this != running && this != compensating;
}

/// A job. The whole server object is in [raw].
class Job {
  /// Creates a job.
  const Job({
    required this.id,
    required this.queue,
    required this.kind,
    required this.status,
    required this.raw,
  });

  /// Reads the server's JSON.
  factory Job.fromJson(Json j) => Job(
        id: j['id'] as String? ?? '',
        queue: j['queue'] as String? ?? '',
        kind: j['kind'] as String? ?? '',
        status: JobStatus.parse(j['status']),
        raw: j,
      );

  /// Job id.
  final String id;

  /// Queue name.
  final String queue;

  /// Handler kind.
  final String kind;

  /// Current state.
  final JobStatus status;

  /// The whole JSON object.
  final Json raw;

  /// Attempts made so far.
  int get attempts => (raw['attempts'] as num?)?.toInt() ?? 0;

  /// The job's result, once it succeeded.
  Object? get result => raw['result'];

  /// Error of the last attempt.
  String? get lastError => raw['last_error'] as String?;

  /// Last progress report (`percent`, `stage`, `message`, `updated_at`).
  Json? get progress => raw['progress'] as Json?;
}

/// A live update delivered on `jobs:<id>`.
class JobUpdate {
  /// Creates an update.
  const JobUpdate({
    required this.event,
    required this.jobId,
    required this.status,
    required this.updatedAt,
    required this.raw,
    this.progress,
  });

  /// Reads the server's JSON.
  factory JobUpdate.fromJson(Json j) => JobUpdate(
        event: j['event'] as String? ?? 'status',
        jobId: j['job_id'] as String? ?? '',
        status: JobStatus.parse(j['status']),
        updatedAt: j['updated_at'] as String? ?? '',
        progress: j['progress'] as Json?,
        raw: j,
      );

  /// Builds the update a stored job stands for.
  factory JobUpdate.fromJob(Job job) => JobUpdate(
        event: 'status',
        jobId: job.id,
        status: job.status,
        updatedAt: job.raw['updated_at'] as String? ?? '',
        progress: job.progress,
        raw: {
          'event': 'status',
          'job_id': job.id,
          'queue': job.queue,
          'kind': job.kind,
          'status': job.status.name,
          'attempts': job.attempts,
          'progress': job.progress,
          'last_error': job.lastError,
          'updated_at': job.raw['updated_at'],
          'finished_at': job.raw['finished_at'],
        },
      );

  /// `progress` (a report or checkpoint) or `status` (a state change).
  final String event;

  /// Job id.
  final String jobId;

  /// State after the update.
  final JobStatus status;

  /// RFC 3339 time.
  final String updatedAt;

  /// `percent`, `stage`, `message`.
  final Json? progress;

  /// The whole JSON object.
  final Json raw;
}

/// A job did not finish in time or ended unsuccessfully ([JobsApi.wait]).
class JobFailedException extends ForgeBaseException {
  /// Creates the exception.
  JobFailedException(this.job, [String? message])
      : super(
          status: 0,
          code: 'job_failed',
          message: message ??
              'job ${job.id} ${job.status.name}'
                  '${job.lastError != null ? ': ${job.lastError}' : ''}',
        );

  /// The job at the time.
  final Job job;
}

ForgeBaseException _wfErr(String message) =>
    ForgeBaseException(status: 0, code: 'invalid_workflow', message: message);

/// Options of one workflow step.
class StepOptions {
  /// Creates options.
  const StepOptions({
    this.after,
    this.queue,
    this.priority,
    this.maxAttempts,
    this.backoffBaseMs,
    this.backoffMaxMs,
    this.visibilityTimeoutMs,
    this.timeoutMs,
    this.compensate,
  });

  /// Steps this one waits for. Default: the step before it; `[]` for a root.
  final List<String>? after;

  /// Queue name.
  final String? queue;

  /// Priority.
  final int? priority;

  /// Attempts before the step is dead.
  final int? maxAttempts;

  /// First retry delay.
  final int? backoffBaseMs;

  /// Longest retry delay.
  final int? backoffMaxMs;

  /// Lease time of one attempt.
  final int? visibilityTimeoutMs;

  /// Longest run of one attempt.
  final int? timeoutMs;

  /// Runs when the workflow fails after this step succeeded
  /// (`onFailure: compensate`): `{kind, payload?, queue?}`.
  final Map<String, Object?>? compensate;
}

/// Builds a workflow (a DAG of jobs).
///
/// ```dart
/// workflow('etl')
///   .step('fetch', 'http.get', {'url': u})
///   .step('load', 'db.load', null, StepOptions(after: ['fetch']));
/// ```
class WorkflowBuilder {
  /// Starts a workflow definition.
  WorkflowBuilder(this.name);

  /// Workflow name.
  final String name;
  final List<Map<String, Object?>> _steps = [];
  String? _policy;

  /// `compensate` runs the steps' `compensate` jobs (newest first) when a
  /// step fails; the default `fail` just stops.
  WorkflowBuilder onFailure(String policy) {
    _policy = policy;
    return this;
  }

  /// Adds a step.
  WorkflowBuilder step(
    String name,
    String kind, [
    Object? payload,
    StepOptions options = const StepOptions(),
  ]) {
    if (_steps.any((s) => s['name'] == name)) {
      throw _wfErr('workflow step "$name" is defined twice');
    }
    final prev = _steps.isEmpty ? null : _steps.last['name'] as String;
    _steps.add({
      'name': name,
      'kind': kind,
      'payload': payload,
      'depends_on': options.after ?? (prev == null ? <String>[] : [prev]),
      if (options.queue != null) 'queue': options.queue,
      if (options.priority != null) 'priority': options.priority,
      if (options.maxAttempts != null) 'max_attempts': options.maxAttempts,
      if (options.backoffBaseMs != null)
        'backoff_base_ms': options.backoffBaseMs,
      if (options.backoffMaxMs != null) 'backoff_max_ms': options.backoffMaxMs,
      if (options.visibilityTimeoutMs != null)
        'visibility_timeout_ms': options.visibilityTimeoutMs,
      if (options.timeoutMs != null) 'timeout_ms': options.timeoutMs,
      if (options.compensate != null) 'compensate': options.compensate,
    });
    return this;
  }

  /// The request body, validated locally (known dependencies, no cycles).
  Map<String, Object?> build() {
    if (_steps.isEmpty) throw _wfErr('a workflow needs at least one step');
    final names = {for (final s in _steps) s['name'] as String};
    for (final s in _steps) {
      for (final d in (s['depends_on'] as List<String>)) {
        if (!names.contains(d)) {
          throw _wfErr('step "${s['name']}" depends on unknown step "$d"');
        }
        if (d == s['name']) {
          throw _wfErr('step "${s['name']}" depends on itself');
        }
      }
    }
    // Kahn's algorithm: every step must be reachable in topological order.
    final left = {
      for (final s in _steps)
        s['name'] as String: (s['depends_on'] as List<String>).length,
    };
    final ready = [
      for (final s in _steps)
        if ((s['depends_on'] as List<String>).isEmpty) s['name'] as String,
    ];
    var seen = 0;
    while (ready.isNotEmpty) {
      final n = ready.removeLast();
      seen++;
      for (final s in _steps) {
        if ((s['depends_on'] as List<String>).contains(n)) {
          final c = left[s['name']]! - 1;
          left[s['name'] as String] = c;
          if (c == 0) ready.add(s['name'] as String);
        }
      }
    }
    if (seen != _steps.length) {
      throw _wfErr('workflow steps contain a dependency cycle');
    }
    return {
      'name': name,
      if (_policy != null) 'on_failure': _policy,
      'steps': [
        for (final s in _steps) {...s}
      ],
    };
  }
}

/// Starts a workflow definition.
WorkflowBuilder workflow(String name) => WorkflowBuilder(name);

/// Jobs and long tasks: enqueue, watch progress live, checkpoints, results,
/// queues and workflows: `client.jobs`.
class JobsApi {
  /// Creates the API.
  JobsApi(this._t, this._rt, {Sleeper? sleep}) : _sleep = sleep ?? defaultSleep;

  final Transport _t;
  final RealtimeClient _rt;
  final Sleeper _sleep;

  // ---- jobs ----

  /// Enqueues a job. With an [idempotencyKey] a repeat returns the existing
  /// job (and is retried safely).
  Future<Job> enqueue(
    String kind, {
    Object? payload,
    String? queue,
    int? priority,
    String? runAt,
    int? delayMs,
    int? maxAttempts,
    int? backoffBaseMs,
    int? backoffMaxMs,
    int? visibilityTimeoutMs,
    String? idempotencyKey,
    int? timeoutMs,
    List<String>? dependsOn,
  }) async =>
      Job.fromJson(
        await _t.requestJson(
          _j,
          method: 'POST',
          body: {
            'kind': kind,
            'payload': payload,
            if (queue != null) 'queue': queue,
            if (priority != null) 'priority': priority,
            if (runAt != null) 'run_at': runAt,
            if (delayMs != null) 'delay_ms': delayMs,
            if (maxAttempts != null) 'max_attempts': maxAttempts,
            if (backoffBaseMs != null) 'backoff_base_ms': backoffBaseMs,
            if (backoffMaxMs != null) 'backoff_max_ms': backoffMaxMs,
            if (visibilityTimeoutMs != null)
              'visibility_timeout_ms': visibilityTimeoutMs,
            if (idempotencyKey != null) 'idempotency_key': idempotencyKey,
            if (timeoutMs != null) 'timeout_ms': timeoutMs,
            if (dependsOn != null) 'depends_on': dependsOn,
          },
          retry: idempotencyKey != null ? null : RetryOptions.none,
        ),
      );

  /// One job.
  Future<Job> get(String id) async =>
      Job.fromJson(await _t.requestJson('$_j/${_enc(id)}'));

  /// Jobs, newest first.
  Future<({List<Job> items, int total})> list({
    JobStatus? status,
    String? queue,
    String? kind,
    int? limit,
    int? offset,
    String? workflowId,
  }) async {
    final r = await _t.requestJson(
      _j,
      query: {
        'status': status?.name,
        'queue': queue,
        'kind': kind,
        'limit': limit,
        'offset': offset,
        'workflow_id': workflowId,
      },
    );
    return (
      items: [
        for (final i in (r['items'] as List<dynamic>? ?? const []))
          Job.fromJson(i as Json),
      ],
      total: (r['total'] as num?)?.toInt() ?? 0,
    );
  }

  /// Cancels a job.
  Future<Job> cancel(String id) async => Job.fromJson(
        await _t.requestJson('$_j/${_enc(id)}/cancel', method: 'POST'),
      );

  /// Retries a dead job by hand.
  Future<Job> retry(String id) async => Job.fromJson(
        await _t.requestJson('$_j/${_enc(id)}/retry', method: 'POST'),
      );

  /// Deletes a finished job.
  Future<void> delete(String id) async {
    await _t.request('$_j/${_enc(id)}', method: 'DELETE');
  }

  /// Totals per state and per queue.
  Future<Json> stats() => _t.requestJson('$_j/stats');

  /// Attempts of a job.
  Future<List<Json>> attempts(String id) async {
    final r = await _t.requestJson('$_j/${_enc(id)}/attempts');
    return [for (final i in (r['items'] as List<dynamic>)) i as Json];
  }

  /// The last checkpoint of a job.
  Future<Json> checkpoint(String id) =>
      _t.requestJson('$_j/${_enc(id)}/checkpoint');

  /// The stored result of a job (`{job_id, stored, size, result}`).
  Future<Json> result(String id) => _t.requestJson('$_j/${_enc(id)}/result');

  /// Child jobs of a job.
  Future<List<Job>> children(String id) async {
    final r = await _t.requestJson('$_j/${_enc(id)}/children');
    return [
      for (final i in (r['items'] as List<dynamic>)) Job.fromJson(i as Json)
    ];
  }

  // ---- live progress ----

  /// Watches one job: [handler] gets the stored state first (so a late
  /// subscriber sees the current progress), then every live update from the
  /// `jobs:<id>` realtime channel.
  Future<Subscription> onProgress(
    String id,
    void Function(JobUpdate update) handler, {
    void Function(Job job)? onDone,
    bool progressOnly = false,
  }) async {
    var seenAt = '';
    void deliver(JobUpdate u) {
      if (progressOnly && u.event != 'progress') return;
      // Older than what the snapshot already showed.
      if (u.updatedAt.isNotEmpty &&
          seenAt.isNotEmpty &&
          u.updatedAt.compareTo(seenAt) < 0) {
        return;
      }
      if (u.updatedAt.isNotEmpty) seenAt = u.updatedAt;
      handler(u);
    }

    final sub = await _rt.subscribe(SubscribeOptions(channel: 'jobs:$id'), (e) {
      if (e is ChannelMessage && e.data is Map<String, dynamic>) {
        deliver(JobUpdate.fromJson(e.data! as Json));
      }
    });
    try {
      final job = await get(id);
      deliver(JobUpdate.fromJob(job));
      if (job.status.isFinal) onDone?.call(job);
    } on Object {
      await sub.unsubscribe();
      rethrow;
    }
    return sub;
  }

  /// Completes when the job succeeds (with the job); fails with
  /// [JobFailedException] when it ends `dead` or `cancelled`, or when
  /// [timeout] passes. Uses plain polling, so it works without a realtime
  /// connection.
  Future<Job> wait(
    String id, {
    Duration poll = const Duration(seconds: 1),
    Duration timeout = const Duration(minutes: 5),
  }) async {
    final deadline = DateTime.now().add(timeout);
    while (true) {
      final job = await get(id);
      if (job.status == JobStatus.succeeded) return job;
      if (job.status == JobStatus.dead || job.status == JobStatus.cancelled) {
        throw JobFailedException(job);
      }
      if (!DateTime.now().isBefore(deadline)) {
        throw JobFailedException(
          job,
          'job $id still ${job.status.name} after ${timeout.inMilliseconds} ms',
        );
      }
      await _sleep(poll);
    }
  }

  // ---- queues ----

  /// Job queues.
  Future<List<Json>> queues() async {
    final r = await _t.requestJson('$_j/queues');
    return [for (final i in (r['items'] as List<dynamic>)) i as Json];
  }

  /// Stops workers taking jobs from a queue.
  Future<Json> pauseQueue(String name) =>
      _t.requestJson('$_j/queues/${_enc(name)}/pause', method: 'POST');

  /// Lets workers take jobs from a queue again.
  Future<Json> resumeQueue(String name) =>
      _t.requestJson('$_j/queues/${_enc(name)}/resume', method: 'POST');

  // ---- workflows ----

  /// Creates a workflow from a builder or a ready spec.
  Future<Json> createWorkflow(Object wf) {
    final Map<String, Object?> body;
    if (wf is WorkflowBuilder) {
      body = wf.build();
    } else if (wf is Map<String, Object?>) {
      body = wf;
    } else {
      throw ArgumentError('expected a WorkflowBuilder or a spec map');
    }
    return _t.requestJson(
      '$_j/workflows',
      method: 'POST',
      body: body,
      retry: RetryOptions.none,
    );
  }

  /// One workflow with its steps.
  Future<Json> workflow(String id) =>
      _t.requestJson('$_j/workflows/${_enc(id)}');

  /// Workflows.
  Future<({List<Json> items, int total})> listWorkflows({
    WorkflowStatus? status,
    int? limit,
    int? offset,
  }) async {
    final r = await _t.requestJson(
      '$_j/workflows',
      query: {'status': status?.name, 'limit': limit, 'offset': offset},
    );
    return (
      items: [
        for (final i in (r['items'] as List<dynamic>? ?? const [])) i as Json,
      ],
      total: (r['total'] as num?)?.toInt() ?? 0,
    );
  }

  /// Cancels a workflow.
  Future<Json> cancelWorkflow(String id) =>
      _t.requestJson('$_j/workflows/${_enc(id)}/cancel', method: 'POST');

  /// Watches a workflow's live updates (`jobs:<workflow id>`); [handler] gets
  /// the current state first.
  Future<Subscription> onWorkflowProgress(
    String id,
    void Function(Json workflow) handler,
  ) async {
    final sub = await _rt.subscribe(SubscribeOptions(channel: 'jobs:$id'), (e) {
      if (e is! ChannelMessage) return;
      final d = e.data;
      if (d is Map<String, dynamic> &&
          d['event'] == 'workflow' &&
          d['workflow'] is Map<String, dynamic>) {
        handler(d['workflow'] as Json);
      }
    });
    try {
      handler(await workflow(id));
    } on Object {
      await sub.unsubscribe();
      rethrow;
    }
    return sub;
  }

  /// Completes when the workflow succeeds; fails when it ends `failed`,
  /// `cancelled` or `compensated`, or when [timeout] passes.
  Future<Json> waitWorkflow(
    String id, {
    Duration poll = const Duration(seconds: 1),
    Duration timeout = const Duration(minutes: 5),
  }) async {
    final deadline = DateTime.now().add(timeout);
    while (true) {
      final wf = await workflow(id);
      final status = WorkflowStatus.parse(wf['status']);
      if (status == WorkflowStatus.succeeded) return wf;
      if (status.isFinal) {
        throw _wfErr(
          'workflow $id ${status.name}'
          '${wf['error'] != null ? ': ${wf['error']}' : ''}',
        );
      }
      if (!DateTime.now().isBefore(deadline)) {
        throw _wfErr(
          'workflow $id still ${status.name} after ${timeout.inMilliseconds} ms',
        );
      }
      await _sleep(poll);
    }
  }
}
