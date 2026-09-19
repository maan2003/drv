// SPDX-License-Identifier: GPL-2.0-only
/* Test module, never part of the production frontend. */
#include <linux/module.h>
#include <linux/etherdevice.h>
#include <linux/delay.h>
#include <net/page_pool/helpers.h>

static const struct net_device_ops pool_test_ops = {};

static int __init pool_test(void)
{
    struct net_device *dev = alloc_netdev(0, "ns3test%d", NET_NAME_UNKNOWN, ether_setup);
    struct page_pool_params params = { .pool_size = 8, .nid = NUMA_NO_NODE };
    struct page_pool *pool;
    struct page *page;
    int ret;
    if (!dev) return -ENOMEM;
    dev->netdev_ops = &pool_test_ops;
    ret = register_netdev(dev);
    if (ret) { free_netdev(dev); return ret; }
    params.netdev = dev;
    pool = page_pool_create(&params);
    if (IS_ERR(pool)) {
        unregister_netdev(dev); free_netdev(dev); return PTR_ERR(pool);
    }
    page = page_pool_alloc_pages(pool, GFP_KERNEL);
    unregister_netdev(dev);
    free_netdev(dev);
    /* Destroy while a page is outstanding, then exercise deferred release. */
    page_pool_destroy(pool);
    if (!page) return -ENOMEM;
    page_pool_put_full_page(pool, page, false);
    msleep(1200);
    pr_info("PASS_PAGE_POOL_UNREGISTER_WITHOUT_LOOPBACK\n");
    return 0;
}
static void __exit pool_test_exit(void) {}
module_init(pool_test);
module_exit(pool_test_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Page-pool unregister/deferred-release regression without native loopback");
