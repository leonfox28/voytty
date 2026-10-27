#include "Bridge.h"
#include <resolv.h>
#include <string.h>

// Read current system DNS, including IPv6 scopes, without private APIs.
int voytty_dns_servers(VoyttyDNSServer *output, int capacity) {
    struct __res_state state = {0};
    if (res_9_ninit(&state) != 0) return 0;
    union res_9_sockaddr_union servers[8];
    int count = res_9_getservers(&state, servers, 8);
    int written = 0;
    for (int i = 0; i < count && i < 8 && written < capacity; i++) {
        VoyttyDNSServer value = {0};
        if (servers[i].sin.sin_family == AF_INET) {
            value.length = 4;
            memcpy(value.address, &servers[i].sin.sin_addr, 4);
        } else if (servers[i].sin6.sin6_family == AF_INET6) {
            value.length = 16;
            value.scope = servers[i].sin6.sin6_scope_id;
            memcpy(value.address, &servers[i].sin6.sin6_addr, 16);
        } else continue;
        output[written++] = value;
    }
    res_9_ndestroy(&state);
    return written;
}
