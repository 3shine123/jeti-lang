typedef void (^blk_t)(void);
static void take(blk_t b) { b(); }
int main(void) {
    take((^void(){ for (int i = 0; i < 3; i++) { ; } }));
    take((^(){ })); 
    return 0;
}
