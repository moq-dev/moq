/* Exercises the installed package end to end over a real QUIC session from plain C: a returned
 * error, a loopback connect naming the package, a frame read through a callback, a cancelled
 * read, a retrying first dial's state, a callback in flight at shutdown, and every callback run
 * on this thread through a custom dispatcher. */

/* clock_gettime and the pthread timed wait under strict C11. */
#define _POSIX_C_SOURCE 200809L

#include <moq/moq.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <pthread.h>
#include <time.h>
#endif

/* Unlike assert, this survives a release build. */
#define CHECK(expr)                                                                \
    do {                                                                           \
        if (!(expr)) {                                                             \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #expr); \
            abort();                                                               \
        }                                                                          \
    } while (0)

/* Unwraps a call the probe expects to succeed. */
static void ok(moq_error *error, const char *what) {
    if (error) {
        fprintf(stderr, "%s failed: %s\n", what, moq_error_message(error));
        abort();
    }
}

/* The host loop: a queue of work that only the main thread runs. */
typedef struct node {
    moq_work *work;
    struct node *next;
} node;

static struct {
#ifdef _WIN32
    SRWLOCK lock;
    CONDITION_VARIABLE ready;
#else
    pthread_mutex_t lock;
    pthread_cond_t ready;
#endif
    node *head;
    node *tail;
    bool closed;
} loop;

static void loop_lock(void) {
#ifdef _WIN32
    AcquireSRWLockExclusive(&loop.lock);
#else
    pthread_mutex_lock(&loop.lock);
#endif
}

static void loop_unlock(void) {
#ifdef _WIN32
    ReleaseSRWLockExclusive(&loop.lock);
#else
    pthread_mutex_unlock(&loop.lock);
#endif
}

/* Waits up to `ms` for a signal; the caller holds the lock. */
static void loop_wait(int ms) {
#ifdef _WIN32
    SleepConditionVariableSRW(&loop.ready, &loop.lock, (DWORD)ms, 0);
#else
    struct timespec until;
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_sec += ms / 1000;
    until.tv_nsec += (long)(ms % 1000) * 1000000L;
    if (until.tv_nsec >= 1000000000L) {
        until.tv_sec++;
        until.tv_nsec -= 1000000000L;
    }
    pthread_cond_timedwait(&loop.ready, &loop.lock, &until);
#endif
}

/* Called from any thread. */
static bool loop_dispatch(void *user_data, moq_work *work) {
    (void)user_data;
    node *item = malloc(sizeof(node));
    CHECK(item);
    item->work = work;
    item->next = NULL;
    loop_lock();
    if (loop.closed) {
        loop_unlock();
        free(item);
        return false;
    }
    if (loop.tail) {
        loop.tail->next = item;
    } else {
        loop.head = item;
    }
    loop.tail = item;
#ifdef _WIN32
    WakeConditionVariable(&loop.ready);
#else
    pthread_cond_signal(&loop.ready);
#endif
    loop_unlock();
    return true;
}

static node *loop_pop(void) {
    node *item = loop.head;
    if (item) {
        loop.head = item->next;
        if (!loop.head) {
            loop.tail = NULL;
        }
    }
    return item;
}

/* Refuses new work and runs what was accepted. */
static void loop_shutdown(void *user_data) {
    (void)user_data;
    loop_lock();
    loop.closed = true;
    loop_unlock();
    for (;;) {
        loop_lock();
        node *item = loop_pop();
        loop_unlock();
        if (!item) {
            return;
        }
        moq_work_run(item->work);
        free(item);
    }
}

/* Runs work on this thread until `*done` or roughly `ms` passes without it; returns `*done`. */
static bool pump(const bool *done, int ms) {
    int waited = 0;
    while (!*done && waited < ms) {
        loop_lock();
        node *item = loop_pop();
        if (!item) {
            loop_wait(10);
            waited += 10;
            item = loop_pop();
        }
        loop_unlock();
        if (item) {
            moq_work_run(item->work);
            free(item);
        }
    }
    return *done;
}

/* Each async result lands in one of these, on this thread. */
typedef struct {
    bool done;
    moq_error *error;
    void *value;
} result;

static void on_value(void *user_data, moq_error *error, void *value) {
    result *out = user_data;
    out->done = true;
    out->error = error;
    out->value = value;
}

static void on_string(void *user_data, moq_error *error, char *value) {
    on_value(user_data, error, value);
}

static void on_request(void *user_data, moq_error *error, moq_request *value) {
    on_value(user_data, error, value);
}

static void on_session(void *user_data, moq_error *error, moq_session *value) {
    on_value(user_data, error, value);
}

static void on_broadcast(void *user_data, moq_error *error, moq_broadcast_consumer *value) {
    on_value(user_data, error, value);
}

static void on_track(void *user_data, moq_error *error, moq_track_consumer *value) {
    on_value(user_data, error, value);
}

static void on_frame(void *user_data, moq_error *error, moq_frame *value) {
    on_value(user_data, error, value);
}

/* Set once moq_shutdown returns, after which no callback may run. */
static bool shut_down = false;

static void on_late(void *user_data, moq_error *error, moq_frame *value) {
    CHECK(!shut_down);
    on_value(user_data, error, value);
}

/* Waits for a result the probe expects to succeed and returns its value. */
static void *await(result *pending, const char *what) {
    if (!pump(&pending->done, 5000)) {
        fprintf(stderr, "%s timed out\n", what);
        abort();
    }
    ok(pending->error, what);
    return pending->value;
}

static void on_state(void *user_data, moq_error *error, moq_connection_state *value) {
    on_value(user_data, error, value);
}

/* Waits for the state after `state`, which it frees. */
static moq_connection_state *next_state(moq_session *session, moq_connection_state *state) {
    result changed = {0};
    moq_task *task = moq_session_state_changed(session, state->revision, on_state, &changed);
    moq_connection_state *next = await(&changed, "state_changed");
    moq_task_free(task);
    moq_connection_state_free(state);
    return next;
}

int main(void) {
#ifdef _WIN32
    InitializeSRWLock(&loop.lock);
    InitializeConditionVariable(&loop.ready);
#else
    pthread_mutex_init(&loop.lock, NULL);
    pthread_cond_init(&loop.ready, NULL);
#endif
    moq_dispatcher dispatcher = {loop_dispatch, loop_shutdown, NULL};
    moq_set_dispatcher(&dispatcher);

    /* Only the defaults carry moq-net's zero max delay; a bare record sets no subscriber limit. */
    moq_subscription *defaults = moq_subscription_defaults();
    CHECK(defaults->max_delay.has_value && defaults->max_delay.value == 0);
    moq_subscription_free(defaults);
    moq_subscription bare = moq_subscription_default();
    CHECK(!bare.max_delay.has_value);

    /* A synchronous error is a returned value with Rust's Display message. */
    moq_server_config unbound_config = moq_server_config_default();
    moq_server *unbound = NULL;
    ok(moq_server_new(&unbound_config, &unbound), "server new");
    moq_string_list *fingerprints = NULL;
    moq_error *error = moq_server_cert_fingerprints(unbound, &fingerprints);
    CHECK(error && error->tag == MOQ_ERROR_BIND);
    CHECK(strcmp(moq_error_message(error), "bind: not listening; call listen() first") == 0);
    moq_error_free(error);
    moq_server_free(unbound);

    /* Publisher: a server whose origin serves one broadcast with one track. */
    moq_origin_config origin_config = moq_origin_config_default();
    moq_origin_producer *origin = moq_origin_producer_new(&origin_config);
    moq_publication *publication = NULL;
    ok(moq_origin_producer_create_broadcast(origin, "probe", &publication), "create_broadcast");
    moq_broadcast_producer *broadcast = moq_publication_broadcast(publication);
    moq_track_info info = moq_track_info_default();
    info.timescale.has_value = true;
    info.timescale.value = 1000000;
    moq_track_producer *track = NULL;
    ok(moq_broadcast_producer_publish_track(broadcast, "data", &info, &track), "publish_track");
    moq_announce_route route = moq_announce_route_default();
    ok(moq_publication_announce(publication, &route), "announce");
    /* Releasing the publication keeps the route: the subscriber below still reaches it. */
    moq_publication_free(publication);

    const char *generate[] = {"localhost"};
    const char *versions[] = {"moq-lite-07-wip"};
    moq_server_config server_config = moq_server_config_default();
    server_config.bind = "127.0.0.1:0";
    server_config.versions.data = versions;
    server_config.versions.len = 1;
    server_config.tls.generate.data = generate;
    server_config.tls.generate.len = 1;
    server_config.publish = origin;
    moq_server *server = NULL;
    ok(moq_server_new(&server_config, &server), "server new");
    result listening = {0};
    moq_task *task = moq_server_listen(server, on_string, &listening);
    char *addr = await(&listening, "listen");
    moq_task_free(task);

    /* Both halves of the handshake are pending at once, and complete on this thread. */
    moq_client_config client_config = moq_client_config_default();
    client_config.tls.insecure = true;
    client_config.versions = server_config.versions;
    client_config.implementation = MOQ_IMPLEMENTATION;
    moq_client *client = NULL;
    ok(moq_client_new(&client_config, &client), "client new");
    char url[128];
    snprintf(url, sizeof(url), "https://%s", addr);
    moq_string_free(addr);
    result accepting = {0};
    result connecting = {0};
    moq_task *accept_task = moq_server_accept(server, on_request, &accepting);
    moq_task *connect_task = moq_client_connect(client, url, on_session, &connecting);
    moq_request *request = await(&accepting, "accept");
    CHECK(request);
    /* The package names itself ahead of moq-ffi, so a server can tell bindings apart. */
    char *implementation = moq_request_peer_implementation(request);
    CHECK(implementation && strncmp(implementation, "moq-c/", 6) == 0);
    moq_string_free(implementation);
    result serving = {0};
    moq_task *serve_task = moq_request_accept(request, NULL, NULL, on_session, &serving);
    moq_session *served = await(&serving, "request accept");
    moq_session *session = await(&connecting, "connect");
    moq_task_free(accept_task);
    moq_task_free(connect_task);
    moq_task_free(serve_task);

    /* Subscriber: resolve the announced broadcast and subscribe to its track. */
    moq_origin_consumer *remote_origin = moq_session_consume(session);
    moq_following *following = NULL;
    ok(moq_origin_consumer_follow(remote_origin, "probe", NULL, &following), "follow");
    result available = {0};
    task = moq_following_available(following, on_broadcast, &available);
    moq_broadcast_consumer *remote = await(&available, "available");
    moq_task_free(task);
    result subscribed = {0};
    task = moq_broadcast_consumer_subscribe_track(remote, "data", NULL, on_track, &subscribed);
    moq_track_consumer *consumer = await(&subscribed, "subscribe_track");
    moq_task_free(task);

    /* Freeing a pending task cancels the read, and its callback never runs. */
    result cancelled = {0};
    task = moq_track_consumer_read_frame(consumer, on_frame, &cancelled);
    CHECK(!pump(&cancelled.done, 50));
    moq_task_free(task);

    /* The consumer survives the cancelled read and delivers the next frame. */
    result reading = {0};
    task = moq_track_consumer_read_frame(consumer, on_frame, &reading);
    const uint8_t hello[] = {'h', 'e', 'l', 'l', 'o'};
    moq_frame frame = moq_frame_default();
    frame.payload.data = hello;
    frame.payload.len = sizeof(hello);
    frame.timestamp_us.has_value = true;
    frame.timestamp_us.value = 1000;
    ok(moq_track_producer_write_frame(track, &frame), "write_frame");
    moq_frame *received = await(&reading, "read_frame");
    moq_task_free(task);
    CHECK(received);
    CHECK(received->payload.len == sizeof(hello));
    CHECK(memcmp(received->payload.data, hello, sizeof(hello)) == 0);
    /* Lite-07 carries both the Implementation parameter and frame timestamps. */
    CHECK(received->timestamp_us.has_value && received->timestamp_us.value == 1000);
    moq_frame_free(received);
    CHECK(!cancelled.done);

    /* Reading past a finished track delivers no frame. */
    ok(moq_track_producer_finish(track), "track finish");
    result ended = {0};
    task = moq_track_consumer_read_frame(consumer, on_frame, &ended);
    CHECK(await(&ended, "read_frame at end") == NULL);
    moq_task_free(task);

    /* An async error arrives through the callback as a value too. */
    moq_client *closed = NULL;
    ok(moq_client_new(&client_config, &closed), "client new");
    moq_client_cancel(closed);
    result refused = {0};
    task = moq_client_connect(closed, url, on_session, &refused);
    CHECK(pump(&refused.done, 5000));
    CHECK(refused.error && refused.error->tag == MOQ_ERROR_CANCELLED);
    moq_error_free(refused.error);
    moq_task_free(task);
    moq_client_free(closed);

    ok(moq_broadcast_producer_close(broadcast), "broadcast close");
    moq_session_cancel(session, 0);
    moq_session_cancel(served, 0);
    moq_server_cancel(server);

    moq_track_consumer_free(consumer);
    moq_broadcast_consumer_free(remote);
    moq_following_free(following);
    moq_origin_consumer_free(remote_origin);
    moq_session_free(session);
    moq_session_free(served);
    moq_request_free(request);
    moq_client_free(client);
    moq_server_free(server);
    moq_track_producer_free(track);
    moq_broadcast_producer_free(broadcast);
    moq_origin_producer_free(origin);

    /* A first dial that keeps failing still returns the session, whose state names each failed
     * attempt while it retries, then ends closed once cancelled; the server is gone, so every
     * dial fails. */
    moq_client_config retry_config = moq_client_config_default();
    retry_config.timeout.has_value = true;
    retry_config.timeout.value = 100000000; /* 100ms */
    retry_config.websocket.enabled.has_value = true;
    retry_config.websocket.enabled.value = false;
    moq_client *retrying = NULL;
    ok(moq_client_new(&retry_config, &retrying), "client new");
    result dialing = {0};
    task = moq_client_connect(retrying, url, on_session, &dialing);
    moq_session *redialing = await(&dialing, "retrying connect");
    moq_task_free(task);
    moq_connection_state *state = moq_session_state(redialing);
    while (!state->failure) {
        state = next_state(redialing, state);
    }
    CHECK(state->phase == MOQ_CONNECTION_PHASE_CONNECTING);
    CHECK(state->failure->tag == MOQ_ERROR_CONNECT);
    CHECK(state->connects == 0);
    moq_session_cancel(redialing, 0);
    while (state->phase != MOQ_CONNECTION_PHASE_CLOSED) {
        state = next_state(redialing, state);
    }
    CHECK(state->connects == 0);
    moq_connection_state_free(state);
    moq_session_free(redialing);
    moq_client_free(retrying);

    /* A read whose frame arrives just before shutdown has its callback in flight, so it may run
     * while moq_shutdown drains the dispatcher; none may run once it returns, even as the
     * handles are freed afterward. */
    moq_origin_producer *late_origin = moq_origin_producer_new(&origin_config);
    moq_publication *late_publication = NULL;
    ok(moq_origin_producer_create_broadcast(late_origin, "late", &late_publication), "late broadcast");
    moq_broadcast_producer *late_broadcast = moq_publication_broadcast(late_publication);
    moq_publication_free(late_publication);
    moq_track_producer *late_track = NULL;
    ok(moq_broadcast_producer_publish_track(late_broadcast, "data", &info, &late_track), "late track");
    moq_track_consumer *late = NULL;
    ok(moq_track_producer_consume(late_track, NULL, &late), "late consume");
    result in_flight = {0};
    task = moq_track_consumer_read_frame(late, on_late, &in_flight);
    ok(moq_track_producer_write_frame(late_track, &frame), "late write");

    /* Stop the runtime and the dispatcher before the process tears down. */
    moq_shutdown();
    shut_down = true;

    /* Shutdown may have cancelled the read before it saw the frame. */
    if (in_flight.done && in_flight.error) {
        CHECK(in_flight.error->tag == MOQ_ERROR_CANCELLED);
        moq_error_free(in_flight.error);
    } else if (in_flight.done) {
        moq_frame_free(in_flight.value);
    }
    moq_task_free(task);
    moq_track_consumer_free(late);
    moq_track_producer_free(late_track);
    moq_broadcast_producer_free(late_broadcast);
    moq_origin_producer_free(late_origin);

    printf("probe: ok\n");
    return 0;
}
