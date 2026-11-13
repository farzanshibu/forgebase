import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

Map<String, dynamic> job(String status, {String id = 'j1', Object? extra}) => {
      'id': id,
      'queue': 'default',
      'kind': 'report',
      'status': status,
      'attempts': 1,
      'updated_at': '2026-01-01T00:00:10Z',
      if (extra != null) 'last_error': extra,
    };

void main() {
  test(
      'enqueue sends only what was set and retries only with an idempotency key',
      () async {
    final h = Harness((r) => ok(job('queued')));
    final j = await h.fb.jobs.enqueue(
      'report',
      payload: {'id': 1},
      queue: 'slow',
      priority: 5,
      delayMs: 100,
      idempotencyKey: 'k1',
      dependsOn: ['a'],
    );
    expect(j.id, 'j1');
    expect(j.status, JobStatus.queued);
    expect(h.server.last.map, {
      'kind': 'report',
      'payload': {'id': 1},
      'queue': 'slow',
      'priority': 5,
      'delay_ms': 100,
      'idempotency_key': 'k1',
      'depends_on': ['a'],
    });

    var n = 0;
    final h2 =
        Harness((r) => ++n < 2 ? apiError(503, 'x', 'x') : ok(job('queued')));
    await h2.fb.jobs.enqueue('x', idempotencyKey: 'k');
    expect(n, 2);
    final h3 = Harness((r) => apiError(503, 'x', 'x'));
    await expectLater(h3.fb.jobs.enqueue('x'), throwsA(isA<ServerException>()));
    expect(h3.server.log.length, 1);
  });

  test('get, list, cancel, retry, delete, children, result', () async {
    final h = Harness((r) {
      if (r.method == 'DELETE') return Reply(204);
      if (r.path == '/api/v1/jobs') {
        return ok({
          'items': [job('running')],
          'total': 9
        });
      }
      if (r.path.endsWith('/children')) {
        return ok({
          'items': [job('queued', id: 'c')],
        });
      }
      return ok(job('running'));
    });
    final jobs = h.fb.jobs;
    expect((await jobs.get('j1')).status, JobStatus.running);
    final l =
        await jobs.list(status: JobStatus.running, workflowId: 'w', limit: 5);
    expect(l.total, 9);
    expect(h.server.last.query,
        {'status': 'running', 'workflow_id': 'w', 'limit': '5'});
    await jobs.cancel('j1');
    expect(h.server.last.path, '/api/v1/jobs/j1/cancel');
    await jobs.retry('j1');
    await jobs.delete('j1');
    expect((await jobs.children('j1')).single.id, 'c');
  });

  group('wait', () {
    test('resolves on success', () async {
      var n = 0;
      final h = Harness((r) => ok(job(++n < 3 ? 'running' : 'succeeded')));
      final j =
          await h.fb.jobs.wait('j1', poll: const Duration(milliseconds: 5));
      expect(j.status, JobStatus.succeeded);
      expect(n, 3);
      expect(h.sleeps, hasLength(2));
    });

    test('fails when the job ends dead or cancelled', () async {
      final h = Harness((r) => ok(job('dead', extra: 'boom')));
      await expectLater(
        h.fb.jobs.wait('j1'),
        throwsA(
          isA<JobFailedException>()
              .having((e) => e.job.status, 'status', JobStatus.dead)
              .having((e) => e.message, 'message', contains('boom')),
        ),
      );
    });

    test('times out', () async {
      final h = Harness((r) => ok(job('running')));
      await expectLater(
        h.fb.jobs.wait('j1', timeout: Duration.zero),
        throwsA(isA<JobFailedException>()),
      );
    });
  });

  group('onProgress', () {
    late WsHub hub;
    Harness make(Handler handler) {
      hub = WsHub();
      return Harness(handler, ws: hub.connect);
    }

    test(
        'delivers the stored state first, then live updates, dropping stale ones',
        () async {
      final h = make((r) => ok(job('running')));
      final updates = <JobUpdate>[];
      Job? done;
      await h.fb.jobs.onProgress(
        'j1',
        updates.add,
        onDone: (j) => done = j,
      );
      expect(updates.single.status, JobStatus.running);
      expect(done, isNull);
      expect(hub.last.sentOfType('subscribe').single['channel'], 'jobs:j1');

      final subId = hub.last.sentOfType('subscribe').single['id'];
      hub.last.serverSend({
        'type': 'message',
        'id': subId,
        'channel': 'jobs:j1',
        'from': 'server',
        'timestamp': 't',
        'data': {
          'event': 'progress',
          'job_id': 'j1',
          'status': 'running',
          'updated_at': '2026-01-01T00:00:20Z',
          'progress': {'percent': 50.0},
        },
      });
      hub.last.serverSend({
        'type': 'message',
        'id': subId,
        'channel': 'jobs:j1',
        'from': 'server',
        'timestamp': 't',
        'data': {
          'event': 'progress',
          'job_id': 'j1',
          'status': 'running',
          'updated_at': '2026-01-01T00:00:01Z', // older than the snapshot
        },
      });
      await waitFor(() => updates.length == 2);
      await Future<void>.delayed(const Duration(milliseconds: 20));
      expect(updates, hasLength(2));
      expect(updates.last.progress!['percent'], 50.0);
      await h.fb.close();
    });

    test(
        'onDone fires for a job that already finished; progressOnly filters statuses',
        () async {
      final h = make((r) => ok(job('succeeded')));
      final updates = <JobUpdate>[];
      Job? done;
      await h.fb.jobs.onProgress('j1', updates.add,
          onDone: (j) => done = j, progressOnly: true);
      expect(done!.status, JobStatus.succeeded);
      expect(updates, isEmpty);
      await h.fb.close();
    });

    test('unsubscribes when the initial fetch fails', () async {
      final h = make((r) => apiError(404, 'not_found', 'x'));
      await expectLater(
        h.fb.jobs.onProgress('nope', (_) {}),
        throwsA(isA<NotFoundException>()),
      );
      await waitFor(() => hub.last.sentOfType('unsubscribe').isNotEmpty);
      await h.fb.close();
    });
  });

  group('workflows', () {
    test('builder chains steps into a pipeline by default', () {
      final spec = workflow('etl')
          .step('fetch', 'http.get', {'u': 1})
          .step('load', 'db.load')
          .step('notify', 'mail', null,
              const StepOptions(after: ['fetch', 'load'], maxAttempts: 2))
          .onFailure('compensate')
          .build();
      expect(spec['name'], 'etl');
      expect(spec['on_failure'], 'compensate');
      final steps = spec['steps']! as List<Map<String, Object?>>;
      expect(steps[0]['depends_on'], <String>[]);
      expect(steps[1]['depends_on'], ['fetch']);
      expect(steps[2]['depends_on'], ['fetch', 'load']);
      expect(steps[2]['max_attempts'], 2);
      expect(steps[0]['payload'], {'u': 1});
    });

    test('rejects empty, duplicate, unknown, self and cyclic dependencies', () {
      expect(() => workflow('x').build(), throwsA(isA<ForgeBaseException>()));
      expect(
        () => workflow('x').step('a', 'k').step('a', 'k'),
        throwsA(isA<ForgeBaseException>()),
      );
      expect(
        () => workflow('x')
            .step('a', 'k', null, const StepOptions(after: ['nope']))
            .build(),
        throwsA(isA<ForgeBaseException>()),
      );
      expect(
        () => workflow('x')
            .step('a', 'k', null, const StepOptions(after: ['a']))
            .build(),
        throwsA(isA<ForgeBaseException>()),
      );
      expect(
        () => workflow('x')
            .step('a', 'k', null, const StepOptions(after: ['b']))
            .step('b', 'k', null, const StepOptions(after: ['a']))
            .build(),
        throwsA(
          isA<ForgeBaseException>()
              .having((e) => e.message, 'message', contains('cycle')),
        ),
      );
    });

    test('create, get, list, cancel and wait', () async {
      var n = 0;
      final h = Harness((r) {
        if (r.method == 'POST' && r.path == '/api/v1/jobs/workflows') {
          return ok({'id': 'w1', 'status': 'running', 'steps': <dynamic>[]});
        }
        if (r.path == '/api/v1/jobs/workflows') {
          return ok({'items': <dynamic>[], 'total': 0});
        }
        if (r.method == 'POST') return ok({'id': 'w1', 'status': 'cancelled'});
        return ok({'id': 'w1', 'status': ++n < 2 ? 'running' : 'succeeded'});
      });
      final wf = await h.fb.jobs.createWorkflow(workflow('x').step('a', 'k'));
      expect(wf['id'], 'w1');
      expect(h.server.last.map['name'], 'x');
      await h.fb.jobs.listWorkflows(status: WorkflowStatus.running);
      expect(h.server.last.query, {'status': 'running'});
      await h.fb.jobs.cancelWorkflow('w1');
      final done = await h.fb.jobs
          .waitWorkflow('w1', poll: const Duration(milliseconds: 1));
      expect(done['status'], 'succeeded');
    });

    test('waitWorkflow fails when the workflow fails', () async {
      final h = Harness(
          (r) => ok({'id': 'w', 'status': 'failed', 'error': 'step b'}));
      await expectLater(
        h.fb.jobs.waitWorkflow('w'),
        throwsA(isA<ForgeBaseException>()
            .having((e) => e.message, 'm', contains('step b'))),
      );
    });
  });

  test('status helpers', () {
    expect(JobStatus.dead.isFinal, isTrue);
    expect(JobStatus.running.isFinal, isFalse);
    expect(WorkflowStatus.compensating.isFinal, isFalse);
    expect(WorkflowStatus.failed.isFinal, isTrue);
  });
}
