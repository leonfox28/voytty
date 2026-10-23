#import <Foundation/Foundation.h>
#import <xpc/xpc.h>
#import <dlfcn.h>

// Host-only acceptance helper for Xcode 27's DTUHID simulator service. This is
// never part of an iOS target. Protocol reference: facebook/idb's
// FBSimulatorControl/HID/DTUHIDModels.swift and XPC/SimulatorXPCConnection.swift.
@interface NSObject (SimulatorKeyboard)
+ (id)sharedServiceContextForDeveloperDir:(NSString *)path error:(NSError **)error;
- (id)defaultDeviceSetWithError:(NSError **)error;
- (NSArray *)availableDevices;
- (NSUUID *)UDID;
- (mach_port_t)lookup:(NSString *)service error:(NSError **)error;
@end

static const char *service = "com.apple.coredevice.feature.remote.hid.digitizer";
static xpc_object_t message(uint64_t usage, uint64_t state, bool barrier) {
    xpc_object_t payload = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_uint64(payload, "usageCode", usage);
    xpc_dictionary_set_uint64(payload, "state", state);
    xpc_object_t msg = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_string(msg, "messageType", "IndigoKeyboardButtonEvent");
    xpc_dictionary_set_string(msg, "featureIdentifier", service);
    xpc_dictionary_set_bool(msg, "isBarrier", barrier);
    xpc_dictionary_set_value(msg, "payload", payload);
    return msg;
}
int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 4) { fprintf(stderr, "usage: simulator-keyboard DEVELOPER_DIR UUID HID_USAGE...\n"); return 2; }
    for (int i = 3; i < argc; i++) {
        char *end = NULL;
        unsigned long usage = strtoul(argv[i], &end, 10);
        if (!argv[i][0] || *end || usage > 231) { fprintf(stderr, "Invalid keyboard usage\n"); return 2; }
    }
    void *framework = dlopen("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator", RTLD_NOW);
    if (!framework) { fprintf(stderr, "%s\n", dlerror()); return 2; }
    NSError *error = nil;
    id context = [NSClassFromString(@"SimServiceContext") sharedServiceContextForDeveloperDir:@(argv[1]) error:&error];
    id set = [context defaultDeviceSetWithError:&error];
    id device = nil;
    for (id candidate in [set availableDevices]) if ([[candidate UDID].UUIDString isEqualToString:@(argv[2])]) { device = candidate; break; }
    if (!device) { NSLog(@"Missing simulator: %@", error); return 2; }
    mach_port_t port = [device lookup:@(service) error:&error];
    if (!port) { NSLog(@"Keyboard service lookup failed: %@", error); return 2; }
    xpc_endpoint_t (*endpoint)(mach_port_t, uint64_t, uint64_t) = dlsym(RTLD_DEFAULT, "xpc_endpoint_create_mach_port_4sim");
    void (*enable)(xpc_connection_t) = dlsym(RTLD_DEFAULT, "xpc_connection_enable_sim2host_4sim");
    if (!endpoint || !enable) { fprintf(stderr, "Xcode lacks simulator XPC support\n"); return 2; }
    xpc_endpoint_t peer = endpoint(port, 0, 0);
    if (!peer) return 2;
    xpc_connection_t connection = xpc_connection_create_from_endpoint(peer);
    if (!connection) return 2;
    enable(connection);
    xpc_connection_set_event_handler(connection, ^(xpc_object_t event) { (void)event; });
    xpc_connection_resume(connection);
    dispatch_semaphore_t ready = dispatch_semaphore_create(0);
    __block bool live = false;
    xpc_connection_send_message_with_reply(connection, message(0, 2, true), dispatch_get_global_queue(0,0), ^(xpc_object_t reply) {
        live = xpc_get_type(reply) != XPC_TYPE_ERROR;
        dispatch_semaphore_signal(ready);
    });
    if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 4 * NSEC_PER_SEC)) || !live) { fprintf(stderr, "Keyboard service did not answer\n"); return 2; }
    usleep(200000);
    for (int i=3; i<argc; i++) {
        uint64_t usage = strtoull(argv[i], NULL, 10);
        xpc_connection_send_message(connection, message(usage, 1, false));
        usleep(60000);
        xpc_connection_send_message(connection, message(usage, 2, false));
        usleep(60000);
    }
    xpc_connection_send_barrier(connection, ^{ dispatch_semaphore_signal(ready); });
    if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 4 * NSEC_PER_SEC))) return 2;
    usleep(100000);
    xpc_connection_cancel(connection);
    return 0;
} }
