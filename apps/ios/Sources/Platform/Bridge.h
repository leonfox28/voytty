#include <stdint.h>
typedef struct {
    uint8_t address[16];
    uint8_t length;
    uint32_t scope;
} VoyttyDNSServer;
int voytty_dns_servers(VoyttyDNSServer *output, int capacity);
