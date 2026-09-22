#include <stdio.h>
#include <stdlib.h>

#define MAX_ITEMS 64

/* A growable list. */
struct list {
    int *items;
    size_t len;
};

typedef struct list list_t;

static int sum(const list_t *l) {
    int total = 0;
    for (size_t i = 0; i < l->len; i++) {
        total += l->items[i];
    }
    return total;
}

int main(int argc, char **argv) {
    list_t l = {0};
    l.items = calloc(MAX_ITEMS, sizeof(int));
    l.len = 0;
    printf("%d\n", sum(&l));
    free(l.items);
    return 0;
}
