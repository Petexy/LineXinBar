#include "rc_hash.h"
int lxb_achievement_hash(char output[33], unsigned console, const char* path,
                         const unsigned char* data, size_t size) {
    rc_hash_iterator_t iterator;
    rc_hash_initialize_iterator(&iterator, path, data, size);
    int result = rc_hash_generate(output, console, &iterator);
    rc_hash_destroy_iterator(&iterator);
    return result;
}
