/// Dart and Flutter SDK for ForgeBase.
///
/// ```dart
/// final fb = ForgeBase('http://localhost:8090');
/// await fb.auth.signIn(email: 'ada@example.com', password: '...');
/// final posts = await fb.collection('posts').list();
/// ```
library;

export 'src/auth.dart'
    show
        AuthApi,
        AuthChange,
        AuthEvent,
        AuthManager,
        AuthOptions,
        MfaChallenge,
        OAuthApi,
        Session,
        SignInOptions,
        SignInResult;
export 'src/billing.dart'
    show BillingApi, BillingStoreApi, StoreProvider, StoreReceipt;
export 'src/client.dart' show ForgeBase;
export 'src/consistency.dart'
    show
        CommitTokens,
        Consistency,
        commitHeader,
        consistencyHeader,
        readAfterHeader;
export 'src/entitlements.dart' show EntitlementsApi, QuotaRequest;
export 'src/errors.dart'
    show
        ApiException,
        AuthenticationException,
        ConflictException,
        FieldError,
        ForgeBaseException,
        InvalidResponseException,
        NetworkException,
        NotFoundException,
        PermissionException,
        QuotaExceededException,
        RateLimitException,
        RealtimeException,
        ServerException,
        ValidationException,
        errorFromResponse,
        parseRetryAfter;
export 'src/functions.dart' show FunctionResponse, FunctionsApi;
export 'src/http.dart'
    show Credentials, Json, RetryOptions, Sleeper, Transport, defaultSleep;
export 'src/jobs.dart'
    show
        Job,
        JobFailedException,
        JobStatus,
        JobUpdate,
        JobsApi,
        StepOptions,
        WorkflowBuilder,
        WorkflowStatus,
        workflow;
export 'src/kv.dart' show KvApi, KvKey, KvMessage, kvDecode, kvEncode;
export 'src/queues.dart'
    show
        AckResult,
        Delivery,
        PublishMessage,
        Published,
        QueueMessage,
        QueueStream,
        QueuesApi,
        ReceivedMessage;
export 'src/realtime.dart'
    show
        ChannelMessage,
        PresenceEvent,
        PresenceMember,
        ReconnectOptions,
        RealtimeClient,
        RealtimeEvent,
        RealtimeHandler,
        RealtimeState,
        RecordEvent,
        RecordEventType,
        ResetEvent,
        SubscribeOptions,
        Subscription,
        TokenSource,
        WebSocketConnector;
export 'src/records.dart'
    show
        CollectionsApi,
        ListOptions,
        ListResult,
        RecordsApi,
        Row,
        TransferFormat;
export 'src/sse.dart' show SseEvent, parseSse;
export 'src/storage.dart'
    show
        Bucket,
        ObjectInfo,
        ObjectPage,
        ObjectStream,
        PartInfo,
        SignedUrl,
        StorageApi,
        Upload,
        keyPath,
        sha256Hex;
export 'src/token_storage.dart'
    show CallbackTokenStorage, MemoryTokenStorage, TokenStorage;
