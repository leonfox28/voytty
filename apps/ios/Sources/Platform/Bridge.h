#include <stdint.h>
typedef struct {
    uint8_t address[16];
    uint8_t length;
    uint32_t scope;
} ZtermDNSServer;
int zterm_dns_servers(ZtermDNSServer *output, int capacity);
